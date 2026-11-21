//! fastText 埋め込みベクトル（`.vec` テキスト形式）を用いた文脈共起プリセット自動拡張モジュール。
//!
//! 既存の `context_frequencies` に登録された確実なアンカーペアから各ターゲット候補（例: 着る/切る/伐る）の
//! プロトタイプ（アンカー集合および重心ベクトル）を構築し、`.vec` ファイルを 2 パスでストリーミング走査して
//! 同音異義語間の競合マージンが十分な関連語彙のみを自動抽出・スコア化する。

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// fastText による文脈共起拡張のパラメータ設定。
#[derive(Debug, Clone)]
pub struct FastTextExpandOptions {
    /// fastText `.vec` ファイルのパス（テキスト形式、ヘッダ行の有無どちらも可）
    pub vec_path: PathBuf,
    /// 語彙フィルタおよび同音グループ補完に使用する SKK 辞書パス（任意、UTF-8 / EUC-JP 自動対応）
    pub dict_path: Option<PathBuf>,
    /// 抽出する最小類似度閾値
    pub min_similarity: f32,
    /// 同一同音異義グループ内で次点ターゲットに対して要求する最小スコア差
    pub min_margin: f32,
    /// 1 ターゲット候補あたりに追加する最大文脈語数
    pub max_per_target: usize,
}

impl Default for FastTextExpandOptions {
    fn default() -> Self {
        Self {
            vec_path: PathBuf::new(),
            dict_path: None,
            min_similarity: 0.58,
            min_margin: 0.04,
            max_per_target: 50,
        }
    }
}

/// fastText 拡張処理の実行統計。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FastTextExpandStats {
    /// `.vec` ベクトルの次元数
    pub dimension: usize,
    /// `.vec` 内にベクトルが存在した一意なアンカー語彙数
    pub matched_anchors: usize,
    /// 拡張対象となったアクティブなターゲット候補数
    pub active_targets: usize,
    /// Pass 2 で類似度評価した有効文脈語数
    pub scanned_words: usize,
    /// 新規追加された `(文脈語, ターゲット候補)` ペア数
    pub added_pairs: usize,
    /// 新規作成された文脈語キー数
    pub added_context_keys: usize,
}

/// Pass 1 で構築するターゲット候補ごとのプロトタイプ表現。
struct TargetPrototype {
    name: String,
    group: String,
    anchor_vecs: Vec<Vec<f32>>,
    centroid: Vec<f32>,
    self_vec: Option<Vec<f32>>,
}

/// 既存の `context_frequencies` をアンカーとして fastText `.vec` から文脈共起ペアを自動拡張する。
///
/// 既存の `(context, target)` ペアおよび既存スコアは維持し、未登録のペアのみを類似度に応じたスコア（5〜9）で追加する。
pub fn expand_context_with_fasttext(
    context_frequencies: &mut HashMap<String, HashMap<String, u64>>,
    opts: &FastTextExpandOptions,
) -> anyhow::Result<FastTextExpandStats> {
    if context_frequencies.is_empty() {
        anyhow::bail!("context_frequencies is empty; anchor entries are required for fastText expansion");
    }
    if opts.max_per_target == 0 {
        return Ok(FastTextExpandStats::default());
    }
    if !opts.min_similarity.is_finite() || !opts.min_margin.is_finite() {
        anyhow::bail!("min_similarity and min_margin must be finite numbers");
    }

    // 1. 既存の context_frequencies から ターゲット -> アンカー集合 を構築
    let mut target_anchors: HashMap<String, Vec<String>> = HashMap::new();
    let mut needed_words: HashSet<String> = HashSet::new();
    for (ctx, cands) in context_frequencies.iter() {
        needed_words.insert(ctx.clone());
        for target in cands.keys() {
            needed_words.insert(target.clone());
            target_anchors
                .entry(target.clone())
                .or_default()
                .push(ctx.clone());
        }
    }
    // 決定論的な順序を保つためアンカーリストをソート・重複排除
    for anchors in target_anchors.values_mut() {
        anchors.sort();
        anchors.dedup();
    }

    let known_targets: HashSet<String> = target_anchors.keys().cloned().collect();

    // 2. SKK 辞書が指定されていれば語彙セットと追加の同音競合グループを読み込む
    let (dict_vocab, dict_groups) = match opts.dict_path {
        Some(ref path) => {
            let (vocab, groups) = load_skk_dict_vocab_and_groups(path, &known_targets)?;
            (Some(vocab), groups)
        }
        None => (None, HashMap::new()),
    };

    // 3. Pass 1: アンカー語およびターゲット自身のベクトルのみ抽出して正規化
    let (dimension, word_vectors) = collect_needed_vectors(&opts.vec_path, &needed_words)?;
    if dimension == 0 {
        anyhow::bail!("fastText vector file contains no valid vector entries");
    }

    let mut matched_anchor_set: HashSet<&str> = HashSet::new();
    let mut target_names: Vec<String> = target_anchors.keys().cloned().collect();
    target_names.sort();

    let mut prototypes: Vec<TargetPrototype> = Vec::new();
    for target in target_names {
        let Some(anchors) = target_anchors.get(&target) else {
            continue;
        };
        let mut anchor_vecs: Vec<Vec<f32>> = Vec::new();
        for a in anchors {
            if let Some(vec) = word_vectors.get(a) {
                matched_anchor_set.insert(a.as_str());
                anchor_vecs.push(vec.clone());
            }
        }
        if anchor_vecs.is_empty() {
            continue;
        }

        let mut centroid = vec![0.0_f32; dimension];
        for v in &anchor_vecs {
            for (c, &x) in centroid.iter_mut().zip(v.iter()) {
                *c += x;
            }
        }
        if !normalize_in_place(&mut centroid) {
            continue;
        }

        let group = resolve_target_group(&target, &dict_groups);
        let self_vec = word_vectors.get(&target).cloned();
        prototypes.push(TargetPrototype {
            name: target,
            group,
            anchor_vecs,
            centroid,
            self_vec,
        });
    }

    let matched_anchors = matched_anchor_set.len();
    let active_targets = prototypes.len();
    if active_targets == 0 {
        return Ok(FastTextExpandStats {
            dimension,
            matched_anchors,
            active_targets: 0,
            scanned_words: 0,
            added_pairs: 0,
            added_context_keys: 0,
        });
    }

    // グループキー -> prototypes インデックス群
    let mut group_members: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, proto) in prototypes.iter().enumerate() {
        group_members
            .entry(proto.group.clone())
            .or_default()
            .push(idx);
    }
    let mut groups: Vec<&[usize]> = group_members.values().map(Vec::as_slice).collect();
    groups.sort_by_key(|m| m[0]);

    // 4. Pass 2: `.vec` を再走査し、各有効文脈語に対するターゲット候補をスコアリング
    let (scanned_words, per_target_candidates) = score_candidates_pass2(
        &opts.vec_path,
        dimension,
        dict_vocab.as_ref(),
        &prototypes,
        &groups,
        opts.min_similarity,
        opts.min_margin,
    )?;

    // 5. 各ターゲットごとに類似度上位 max_per_target 件（既存登録済みペア除く）を反映
    let mut added_pairs = 0usize;
    let mut added_context_keys = 0usize;

    for (target_idx, mut cands) in per_target_candidates.into_iter().enumerate() {
        let target_name = &prototypes[target_idx].name;
        cands.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then_with(|| a.0.cmp(&b.0))
        });

        let mut taken = 0usize;
        for (word, sim) in cands {
            if taken >= opts.max_per_target {
                break;
            }
            if context_frequencies
                .get(&word)
                .is_some_and(|m| m.contains_key(target_name))
            {
                // 既存のアンカー（スコア10等）は保持し、新規追加枠も消費しない
                continue;
            }

            let score = ((sim * 10.0).round() as u64).clamp(5, 9);
            let is_new_key = !context_frequencies.contains_key(&word);
            let entry = context_frequencies.entry(word).or_default();
            entry.insert(target_name.clone(), score);
            if is_new_key {
                added_context_keys += 1;
            }
            added_pairs += 1;
            taken += 1;
        }
    }

    Ok(FastTextExpandStats {
        dimension,
        matched_anchors,
        active_targets,
        scanned_words,
        added_pairs,
        added_context_keys,
    })
}

/// Pass 1: `.vec` ファイルから `needed_words` に含まれる語彙の正規化済みベクトルのみを収集する。
fn collect_needed_vectors(
    vec_path: &Path,
    needed_words: &HashSet<String>,
) -> anyhow::Result<(usize, HashMap<String, Vec<f32>>)> {
    let file = File::open(vec_path)?;
    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut line = String::new();
    let mut expected_dim: Option<usize> = None;
    let mut vectors: HashMap<String, Vec<f32>> = HashMap::with_capacity(needed_words.len());
    let mut is_first_nonempty = true;
    let mut buf = Vec::new();

    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.trim().is_empty() {
            continue;
        }

        if is_first_nonempty {
            is_first_nonempty = false;
            if let Some(dim) = try_parse_vec_header(trimmed) {
                expected_dim = Some(dim);
                continue;
            }
        }

        let Some((word, rest)) = split_word_and_vector(trimmed) else {
            continue;
        };

        // 次元数が未確定（ヘッダなし形式）の場合のみ、先頭の有効行で次元を確定させる
        if expected_dim.is_none() {
            if parse_normalized_vector(rest, None, &mut buf) {
                expected_dim = Some(buf.len());
                if needed_words.contains(word) {
                    vectors.entry(word.to_string()).or_insert_with(|| buf.clone());
                }
            }
            continue;
        }

        if !needed_words.contains(word) {
            continue;
        }

        if parse_normalized_vector(rest, expected_dim, &mut buf) {
            vectors.entry(word.to_string()).or_insert_with(|| buf.clone());
        }
    }

    Ok((expected_dim.unwrap_or(0), vectors))
}

/// Pass 2: `.vec` を再走査し、各有効文脈語とアクティブターゲット群の類似度・競合マージンを判定する。
type PerTargetCandidates = Vec<Vec<(String, f32)>>;

fn score_candidates_pass2(
    vec_path: &Path,
    dimension: usize,
    dict_vocab: Option<&HashSet<String>>,
    prototypes: &[TargetPrototype],
    groups: &[&[usize]],
    min_similarity: f32,
    min_margin: f32,
) -> anyhow::Result<(usize, PerTargetCandidates)> {
    let file = File::open(vec_path)?;
    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut line = String::new();
    let mut is_first_nonempty = true;
    let mut buf = Vec::with_capacity(dimension);
    let mut scores = vec![0.0_f32; prototypes.len()];
    let mut per_target_candidates: Vec<Vec<(String, f32)>> = vec![Vec::new(); prototypes.len()];
    let mut scanned_words = 0usize;
    let mut seen_words: HashSet<String> = HashSet::new();

    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.trim().is_empty() {
            continue;
        }

        if is_first_nonempty {
            is_first_nonempty = false;
            if try_parse_vec_header(trimmed).is_some() {
                continue;
            }
        }

        let Some((word, rest)) = split_word_and_vector(trimmed) else {
            continue;
        };

        if !is_valid_context_word(word) {
            continue;
        }
        if let Some(vocab) = dict_vocab
            && !vocab.contains(word)
        {
            continue;
        }
        if seen_words.contains(word) {
            continue;
        }
        if !parse_normalized_vector(rest, Some(dimension), &mut buf) {
            continue;
        }

        seen_words.insert(word.to_string());
        scanned_words += 1;

        let mut global_max = f32::NEG_INFINITY;
        for (idx, proto) in prototypes.iter().enumerate() {
            let mut max1 = f32::NEG_INFINITY;
            let mut max2 = f32::NEG_INFINITY;
            for anchor_vec in &proto.anchor_vecs {
                let s = dot_product(&buf, anchor_vec);
                if s > max1 {
                    max2 = max1;
                    max1 = s;
                } else if s > max2 {
                    max2 = s;
                }
            }
            let top_anchor_sim = if proto.anchor_vecs.len() >= 2 {
                0.60 * max1 + 0.40 * max2
            } else {
                max1
            };
            let centroid_sim = dot_product(&buf, &proto.centroid);
            let combined = if let Some(ref self_vec) = proto.self_vec {
                let self_sim = dot_product(&buf, self_vec).max(0.0);
                0.55 * top_anchor_sim + 0.30 * centroid_sim + 0.15 * self_sim
            } else {
                0.65 * top_anchor_sim + 0.35 * centroid_sim
            };
            scores[idx] = combined;
            if combined > global_max {
                global_max = combined;
            }
        }

        if !global_max.is_finite() || global_max < min_similarity {
            continue;
        }

        let inter_group_margin = (min_margin * 3.0).max(0.12);
        let mut group_winners: Vec<(usize, f32)> = Vec::new();
        for &members in groups {
            let mut best_idx = members[0];
            let mut best_score = scores[best_idx];
            let mut second_score = f32::NEG_INFINITY;

            for &m_idx in &members[1..] {
                let s = scores[m_idx];
                if s > best_score {
                    second_score = best_score;
                    best_score = s;
                    best_idx = m_idx;
                } else if s > second_score {
                    second_score = s;
                }
            }

            if best_score < min_similarity {
                continue;
            }
            if members.len() > 1 && (best_score - second_score) < min_margin {
                continue;
            }
            if best_score >= global_max - inter_group_margin {
                group_winners.push((best_idx, best_score));
            }
        }

        if group_winners.is_empty() {
            continue;
        }

        group_winners.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for (target_idx, sim) in group_winners.into_iter().take(3) {
            per_target_candidates[target_idx].push((word.to_string(), sim));
        }
    }

    Ok((scanned_words, per_target_candidates))
}

/// fastText `.vec` 先頭ヘッダ行（`<num_words> <dim>`）であれば次元数を返す。
fn try_parse_vec_header(line: &str) -> Option<usize> {
    let mut parts = line.split_ascii_whitespace();
    let first = parts.next()?;
    let second = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let _count: usize = first.parse().ok()?;
    let dim: usize = second.parse().ok()?;
    if dim == 0 {
        return None;
    }
    Some(dim)
}

/// `.vec` の1行を単語とベクトル成分文字列に分割する。
fn split_word_and_vector(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim_end();
    let idx = trimmed.find(|c: char| c.is_ascii_whitespace())?;
    let word = &trimmed[..idx];
    let rest = trimmed[idx..].trim_start();
    if word.is_empty() || rest.is_empty() {
        return None;
    }
    Some((word, rest))
}

/// 空白区切り浮動小数点列をパースして L2 正規化し、`out` に格納する。
fn parse_normalized_vector(rest: &str, expected_dim: Option<usize>, out: &mut Vec<f32>) -> bool {
    out.clear();
    if let Some(dim) = expected_dim {
        out.reserve(dim.saturating_sub(out.capacity()));
    }
    for token in rest.split_ascii_whitespace() {
        let Ok(val) = token.parse::<f32>() else {
            return false;
        };
        if !val.is_finite() {
            return false;
        }
        out.push(val);
        if let Some(dim) = expected_dim
            && out.len() > dim
        {
            return false;
        }
    }
    if out.is_empty() {
        return false;
    }
    if let Some(dim) = expected_dim
        && out.len() != dim
    {
        return false;
    }
    normalize_in_place(out)
}

#[inline]
fn normalize_in_place(vec: &mut [f32]) -> bool {
    let mut sq_sum = 0.0_f64;
    for &x in vec.iter() {
        let xf = x as f64;
        sq_sum += xf * xf;
    }
    if !sq_sum.is_finite() || sq_sum <= 1e-12 {
        return false;
    }
    let inv_norm = (1.0 / sq_sum.sqrt()) as f32;
    for x in vec.iter_mut() {
        *x *= inv_norm;
    }
    true
}

#[inline]
fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = 0.0_f32;
    for (&x, &y) in a.iter().zip(b.iter()) {
        acc += x * y;
    }
    acc
}

#[inline]
fn is_katakana_char(c: char) -> bool {
    ('\u{30A1}'..='\u{30F6}').contains(&c) || c == 'ー'
}

/// 文脈語として妥当な名詞候補か判定する。
/// 2 文字以上で漢字またはカタカナを 1 文字以上含み、1 文字多義語・ASCII 記号・空白・未検証送り仮名断片を除外する。
pub fn is_valid_context_word(word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let mut char_count = 0usize;
    let mut has_kanji_or_katakana = false;
    for c in word.chars() {
        if c.is_whitespace() || c.is_control() || c.is_ascii_punctuation() {
            return false;
        }
        char_count += 1;
        if crate::okuri::is_kanji(c) || is_katakana_char(c) {
            has_kanji_or_katakana = true;
        }
    }
    if char_count < 2 || !has_kanji_or_katakana {
        return false;
    }
    if crate::frequency::is_unverified_okuri_fragment(word) {
        return false;
    }
    true
}

/// 組み込みテーブルと辞書由来情報から、ターゲット候補の同音異義競合グループキーを決定する。
fn resolve_target_group(target: &str, dict_groups: &HashMap<String, String>) -> String {
    if let Some(static_group) = homophone_group_key(target) {
        return static_group.to_string();
    }
    if let Some(dict_group) = dict_groups.get(target) {
        return dict_group.clone();
    }
    target.to_string()
}

/// SKK 変換で同一見出し語から競合する主要な用言・同音異義グループを返す。
pub fn homophone_group_key(target: &str) -> Option<&'static str> {
    let key = match target {
        "着る" | "切る" | "伐る" | "斬る" => "きる",
        "計る" | "測る" | "量る" | "図る" | "謀る" | "諮る" => "はかる",
        "観る" | "診る" | "看る" | "見る" => "みる",
        "解く" | "梳く" | "溶く" => "とく",
        "乗る" | "載る" => "のる",
        "撮る" | "採る" | "捕る" | "取る" | "執る" => "とる",
        "建てる" | "立てる" => "たてる",
        "建つ" | "立つ" | "断つ" | "絶つ" | "経つ" | "裁つ" => "たつ",
        "書く" | "描く" | "掻く" | "欠く" => "かく",
        "降る" | "振る" => "ふる",
        "会う" | "遭う" | "合う" => "あう",
        "納める" | "収める" | "修める" | "治める" => "おさめる",
        "作る" | "造る" | "創る" => "つくる",
        "直す" | "治す" => "なおす",
        "送る" | "贈る" => "おくる",
        "開ける" | "空ける" | "明ける" => "あける",
        "開く" | "空く" | "明く" => "あく",
        "閉める" | "締める" | "絞める" => "しめる",
        "指す" | "刺す" | "差す" | "挿す" => "さす",
        "引く" | "弾く" | "挽く" | "轢く" => "ひく",
        "張る" | "貼る" => "はる",
        "点ける" | "付ける" | "着ける" | "漬ける" | "就く" | "着く" | "突く" => "つける",
        "打つ" | "撃つ" | "討つ" => "うつ",
        "泣く" | "鳴く" => "なく",
        "咲く" | "裂く" | "割く" => "さく",
        "買う" | "飼う" => "かう",
        "変える" | "換える" | "替える" | "代える" => "かえる",
        "返る" | "帰る" | "還る" => "かえる_五段",
        "返す" | "帰す" | "還す" => "かえす",
        "当てる" | "充てる" => "あてる",
        "置く" | "起く" => "おく",
        "止める" | "留める" | "泊める" => "とめる",
        "止まる" | "留まる" | "泊まる" => "とまる",
        "伸びる" | "延びる" => "のびる",
        "伸ばす" | "延ばす" => "のばす",
        "上げる" | "挙げる" | "揚げる" => "あげる",
        "押さえる" | "抑える" => "おさえる",
        "掛ける" | "架ける" | "賭ける" | "欠ける" => "かける",
        "聞く" | "聴く" | "効く" | "利く" => "きく",
        "移す" | "写す" | "映す" => "うつす",
        "下ろす" | "降ろす" | "卸す" => "おろす",
        "捜す" | "探す" => "さがす",
        "進める" | "勧める" | "奨める" => "すすめる",
        "努める" | "務める" | "勤める" => "つとめる",
        "履く" | "吐く" | "掃く" => "はく",
        "離す" | "放す" => "はなす",
        "払う" | "祓う" => "はらう",
        "掘る" | "彫る" => "ほる",
        "巻く" | "蒔く" | "撒く" => "まく",
        "守る" | "護る" => "まもる",
        "読む" | "詠む" => "よむ",
        "沸く" | "湧く" => "わく",
        "分ける" | "別ける" => "わける",
        "越える" | "超える" => "こえる",
        "入れる" | "容れる" | "淹れる" => "いれる",
        "合わせる" | "併せる" => "あわせる",
        "追う" | "負う" => "おう",
        "固める" | "堅める" => "かためる",
        "沈める" | "鎮める" => "しずめる",
        "攻める" | "責める" => "せめる",
        "使う" | "遣う" => "つかう",
        "積む" | "摘む" => "つむ",
        "飛ぶ" | "跳ぶ" => "とぶ",
        "登る" | "上る" | "昇る" => "のぼる",
        "増やす" | "殖やす" => "ふやす",
        "許す" | "赦す" => "ゆるす",
        _ => return None,
    };
    Some(key)
}

/// SKK 辞書（UTF-8 / EUC-JP）を読み込み、語彙セットおよび同音見出しによるターゲット競合グループを構築する。
fn load_skk_dict_vocab_and_groups(
    dict_path: &Path,
    known_targets: &HashSet<String>,
) -> anyhow::Result<(HashSet<String>, HashMap<String, String>)> {
    let bytes = std::fs::read(dict_path)?;
    let content = match std::str::from_utf8(&bytes) {
        Ok(s) => std::borrow::Cow::Borrowed(s),
        Err(_) => {
            let (decoded, _, _) = encoding_rs::EUC_JP.decode(&bytes);
            decoded
        }
    };

    let mut vocab = HashSet::new();
    let mut dict_groups: HashMap<String, String> = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        let Some((midashi, cands_part)) = line.split_once(' ') else {
            continue;
        };
        if midashi.is_empty() || !cands_part.starts_with('/') || !cands_part.ends_with('/') {
            continue;
        }

        let candidates = crate::frequency::parse_dict_line_candidates(cands_part);
        let mut matched_targets: Vec<String> = Vec::new();
        let okuri_suffix = trailing_okuri_suffix(midashi);

        for cand in candidates {
            if is_valid_context_word(&cand) {
                vocab.insert(cand.clone());
            }
            if known_targets.contains(&cand) {
                matched_targets.push(cand.clone());
            }
            if okuri_suffix.is_some()
                && let Some(expanded) = reconstruct_target_from_okuri_stem(&cand, cands_part, known_targets)
            {
                matched_targets.push(expanded);
            }
        }

        if matched_targets.len() >= 2 {
            matched_targets.sort();
            matched_targets.dedup();
            if matched_targets.len() >= 2 {
                let group_name = format!("dict:{midashi}");
                for t in matched_targets {
                    dict_groups.entry(t).or_insert_with(|| group_name.clone());
                }
            }
        }
    }

    Ok((vocab, dict_groups))
}

/// 送りあり見出し語（末尾が ASCII 英字）から送りキーを取得する。
fn trailing_okuri_suffix(midashi: &str) -> Option<char> {
    let last = midashi.chars().next_back()?;
    if last.is_ascii_alphabetic() && midashi.chars().count() >= 2 {
        Some(last.to_ascii_lowercase())
    } else {
        None
    }
}

/// 送りブロック内に含まれる送り仮名と語幹候補から既知ターゲット終止形を照合する。
fn reconstruct_target_from_okuri_stem(
    stem: &str,
    cands_part: &str,
    known_targets: &HashSet<String>,
) -> Option<String> {
    let trimmed = cands_part.trim_matches('/');
    let mut current_okuri: Option<&str> = None;
    for part in trimmed.split('/') {
        let mut p = part;
        let starts = p.starts_with('[');
        let ends = p.ends_with(']');
        if starts {
            p = &p[1..];
            if ends && p.len() >= 2 {
                p = &p[..p.len() - 1];
            }
            current_okuri = Some(p);
        } else if let Some(okuri) = current_okuri {
            let candidate_text = if ends {
                current_okuri = None;
                &p[..p.len() - 1]
            } else {
                p
            };
            if crate::frequency::clean_candidate(candidate_text) == stem {
                let full = format!("{stem}{okuri}");
                if known_targets.contains(&full) {
                    return Some(full);
                }
            }
        }
        if ends && !starts {
            current_okuri = None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_vec_file(with_header: bool, entries: &[(&str, &[f32])]) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        if with_header {
            let dim = entries.first().map(|(_, v)| v.len()).unwrap_or(0);
            writeln!(file, "{} {}", entries.len(), dim).unwrap();
        }
        for (word, vec) in entries {
            write!(file, "{word}").unwrap();
            for val in *vec {
                write!(file, " {val:.6}").unwrap();
            }
            writeln!(file).unwrap();
        }
        file
    }

    fn seed_anchor_context() -> HashMap<String, HashMap<String, u64>> {
        let mut ctx: HashMap<String, HashMap<String, u64>> = HashMap::new();
        for (noun, verb) in [
            ("服", "着る"),
            ("シャツ", "着る"),
            ("肉", "切る"),
            ("紙", "切る"),
            ("木", "伐る"),
            ("樹木", "伐る"),
            ("家", "建てる"),
            ("ビル", "建てる"),
            ("計画", "立てる"),
            ("予定", "立てる"),
        ] {
            ctx.entry(noun.to_string())
                .or_default()
                .insert(verb.to_string(), 10);
        }
        ctx
    }

    #[test]
    fn test_fasttext_expansion_separates_homophones_and_rejects_ambiguous() {
        // 6次元空間で領域ごとに直交寄りのプロトタイプを定義:
        // [0]: 衣類(着る), [1]: 切断(切る), [2]: 林業(伐る), [3]: 建築(建てる), [4]: 企画(立てる)
        let entries: &[(&str, &[f32])] = &[
            ("服", &[1.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            ("シャツ", &[0.96, 0.05, 0.0, 0.0, 0.0, 0.1]),
            ("パーカー", &[0.95, 0.02, 0.0, 0.0, 0.0, 0.2]), // 着る に強く近接
            ("肉", &[0.0, 1.0, 0.0, 0.0, 0.0, 0.0]),
            ("紙", &[0.02, 0.96, 0.05, 0.0, 0.0, 0.0]),
            ("ロープ", &[0.0, 0.93, 0.10, 0.0, 0.0, 0.2]), // 切る に強く近接
            ("木", &[0.0, 0.0, 1.0, 0.0, 0.0, 0.0]),
            ("樹木", &[0.0, 0.05, 0.96, 0.0, 0.0, 0.1]),
            ("雑木林", &[0.0, 0.05, 0.94, 0.0, 0.0, 0.2]), // 伐る に強く近接
            // 曖昧語（「切る」と「伐る」のちょうど中間、マージン不足で除外されるべき）
            ("境界物", &[0.0, 0.70, 0.69, 0.0, 0.0, 0.0]),
            ("家", &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
            ("ビル", &[0.0, 0.0, 0.0, 0.95, 0.0, 0.1]),
            ("マンション", &[0.0, 0.0, 0.0, 0.92, 0.02, 0.2]), // 建てる
            ("計画", &[0.0, 0.0, 0.0, 0.0, 1.0, 0.0]),
            ("予定", &[0.0, 0.0, 0.0, 0.0, 0.96, 0.1]),
            ("目標", &[0.0, 0.0, 0.0, 0.0, 0.93, 0.2]), // 立てる
            // ノイズ語彙（1文字仮名、ASCII英字のみ、記号混入、送り断片）
            ("あ", &[1.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            ("ア", &[1.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            ("shirt", &[1.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            ("右や", &[1.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            ("服!", &[1.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
        ];

        for with_header in [true, false] {
            let vec_file = write_vec_file(with_header, entries);
            let mut ctx = seed_anchor_context();
            let opts = FastTextExpandOptions {
                vec_path: vec_file.path().to_path_buf(),
                dict_path: None,
                min_similarity: 0.58,
                min_margin: 0.04,
                max_per_target: 50,
            };

            let stats = expand_context_with_fasttext(&mut ctx, &opts).unwrap();
            assert_eq!(stats.dimension, 6);
            assert_eq!(stats.active_targets, 5);
            assert_eq!(stats.matched_anchors, 10);

            // 新規抽出ペアの検証
            assert!(ctx["パーカー"]["着る"] >= 5 && ctx["パーカー"]["着る"] <= 9);
            assert!(!ctx["パーカー"].contains_key("切る"));
            assert!(ctx["ロープ"]["切る"] >= 5);
            assert!(ctx["雑木林"]["伐る"] >= 5);
            assert!(ctx["マンション"]["建てる"] >= 5);
            assert!(ctx["目標"]["立てる"] >= 5);

            // 曖昧語・ノイズ語は除外されること
            assert!(!ctx.contains_key("境界物"));
            assert!(!ctx.contains_key("あ"));
            assert!(!ctx.contains_key("ア"));
            assert!(!ctx.contains_key("shirt"));
            assert!(!ctx.contains_key("右や"));
            assert!(!ctx.contains_key("服!"));

            // 既存アンカーのスコア 10 が上書きされず維持されること
            assert_eq!(ctx["服"]["着る"], 10);
            assert_eq!(ctx["肉"]["切る"], 10);
        }
    }

    #[test]
    fn test_fasttext_expansion_with_skk_dict_filter() {
        let entries: &[(&str, &[f32])] = &[
            ("服", &[1.0, 0.0, 0.0]),
            ("シャツ", &[0.98, 0.0, 0.1]),
            ("パーカー", &[0.96, 0.0, 0.1]),
            ("非辞書造語", &[0.99, 0.0, 0.05]), // SKK 辞書に未登録のため除外される
            ("肉", &[0.0, 1.0, 0.0]),
            ("紙", &[0.0, 0.98, 0.1]),
        ];
        let vec_file = write_vec_file(true, entries);

        let mut dict_file = NamedTempFile::new().unwrap();
        writeln!(dict_file, ";; okuri-nasi entries.").unwrap();
        writeln!(dict_file, "ぱーかー /パーカー/").unwrap();
        writeln!(dict_file, "ふく /服/").unwrap();

        let mut ctx: HashMap<String, HashMap<String, u64>> = HashMap::new();
        ctx.entry("服".to_string())
            .or_default()
            .insert("着る".to_string(), 10);
        ctx.entry("シャツ".to_string())
            .or_default()
            .insert("着る".to_string(), 10);
        ctx.entry("肉".to_string())
            .or_default()
            .insert("切る".to_string(), 10);
        ctx.entry("紙".to_string())
            .or_default()
            .insert("切る".to_string(), 10);

        let opts = FastTextExpandOptions {
            vec_path: vec_file.path().to_path_buf(),
            dict_path: Some(dict_file.path().to_path_buf()),
            min_similarity: 0.58,
            min_margin: 0.04,
            max_per_target: 10,
        };

        let stats = expand_context_with_fasttext(&mut ctx, &opts).unwrap();
        assert!(ctx.contains_key("パーカー"));
        assert!(!ctx.contains_key("非辞書造語"));
        assert_eq!(stats.added_pairs, 1);
    }

    #[test]
    fn test_is_valid_context_word_filters() {
        assert!(is_valid_context_word("洋服"));
        assert!(is_valid_context_word("樹木"));
        assert!(is_valid_context_word("パーカー"));
        assert!(is_valid_context_word("コンピュータ"));
        assert!(is_valid_context_word("カーディガン"));
        assert!(!is_valid_context_word("木"));
        assert!(!is_valid_context_word("服"));
        assert!(!is_valid_context_word(""));
        assert!(!is_valid_context_word("あ"));
        assert!(!is_valid_context_word("ア"));
        assert!(!is_valid_context_word("word"));
        assert!(!is_valid_context_word("1234"));
        assert!(!is_valid_context_word("貸そ"));
        assert!(!is_valid_context_word("右や"));
        assert!(!is_valid_context_word("着る"));
        assert!(!is_valid_context_word("服,"));
    }
}
