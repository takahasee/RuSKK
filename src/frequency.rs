use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use tracing::debug;

/// 読み取り専用の頻度・文脈データに基づいて候補を並び替える。
/// skkserv プロトコルではユーザーの選択を知る手段がないため、自動学習は行わない。
/// ユーザーが `~/.ruskkserv-frequency.json` を手動で編集し、seed データを投入する。
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct FrequencyPredictor {
    /// 見出し語 → (候補 → 出現回数) のマッピング
    pub frequencies: HashMap<String, HashMap<String, u64>>,
    /// 文脈語（漢字/単語） → (候補 → 共起回数) のマッピング
    #[serde(default)]
    pub context_frequencies: HashMap<String, HashMap<String, u64>>,
    #[serde(skip)]
    pub storage_path: Option<PathBuf>,
}

impl FrequencyPredictor {
    pub fn new(storage_path: Option<PathBuf>) -> Self {
        let mut predictor = Self {
            frequencies: HashMap::new(),
            context_frequencies: HashMap::new(),
            storage_path: None,
        };
        if let Some(ref path) = storage_path
            && let Err(err) = predictor.load(path)
        {
            if path.exists() {
                tracing::warn!(error = %err, path = ?path, "existing frequency data file failed to load (syntax error?) — starting fresh without modifying file");
            } else {
                debug!(error = %err, "no existing frequency data loaded, starting fresh");
            }
        }
        predictor.storage_path = storage_path;
        predictor
    }

    /// ファイル読み込みやエイリアス展開を行わずに、保存先パスだけを設定した空のインスタンスを生成する。
    /// `init-seed` や `import-user-dict` など、直後に生データ読込・上書きを行う CLI サブコマンド用。
    pub fn new_unloaded(storage_path: Option<PathBuf>) -> Self {
        Self {
            frequencies: HashMap::new(),
            context_frequencies: HashMap::new(),
            storage_path,
        }
    }

    pub fn load<P: AsRef<Path>>(&mut self, path: P) -> anyhow::Result<()> {
        if !path.as_ref().exists() {
            return Ok(());
        }
        let data = fs::read_to_string(path)?;
        let loaded: FrequencyPredictor = serde_json::from_str(&data)?;
        self.frequencies = loaded.frequencies;
        self.context_frequencies = loaded.context_frequencies;
        self.expand_aliases();
        Ok(())
    }

    /// 永続化ファイルが存在し非空の場合、エイリアス展開前の生データを再読み込みする。
    /// ファイル保存時（init-seed / import-user-dict）にランタイム用のエイリアスが書き出されるのを防ぐ。
    /// ファイルが存在するのに構文エラーがある場合は、誤上書きを防ぐためエラーを返す。
    fn reload_raw_if_exists(&mut self) -> anyhow::Result<()> {
        if let Some(ref path) = self.storage_path
            && path.exists()
        {
            let data = fs::read_to_string(path)?;
            if !data.trim().is_empty() {
                let loaded: FrequencyPredictor = serde_json::from_str(&data)?;
                self.frequencies = loaded.frequencies;
                self.context_frequencies = loaded.context_frequencies;
            }
        }
        Ok(())
    }

    pub fn save(&self) -> anyhow::Result<()> {
        use std::io::Write;
        use std::sync::atomic::{AtomicU64, Ordering};

        static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

        if let Some(ref path) = self.storage_path {
            let parent = match path.parent() {
                Some(p) if !p.as_os_str().is_empty() => p,
                _ => Path::new("."),
            };
            fs::create_dir_all(parent)?;

            let data = serde_json::to_string_pretty(&self)?;
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("ruskkserv-frequency.json");
            let pid = std::process::id();
            let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let tmp_path = parent.join(format!(".{file_name}.tmp.{pid}.{seq}"));

            let mut opts = fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }

            let write_result = (|| -> anyhow::Result<()> {
                let mut file = opts.open(&tmp_path)?;
                file.write_all(data.as_bytes())?;
                file.sync_all()?;
                Ok(())
            })();

            if let Err(err) = write_result {
                let _ = fs::remove_file(&tmp_path);
                return Err(err);
            }

            if let Err(err) = fs::rename(&tmp_path, path) {
                let _ = fs::remove_file(&tmp_path);
                return Err(err.into());
            }
        }
        Ok(())
    }

    pub const DEFAULT_SEED_JSON: &'static str = include_str!("../data/default-seed.json");

    /// 組み込みのデフォルト文脈プリセットをパースして返す。
    pub fn default_preset_data() -> anyhow::Result<FrequencyPredictor> {
        let data: FrequencyPredictor = serde_json::from_str(Self::DEFAULT_SEED_JSON)?;
        Ok(data)
    }

    /// エイリアス展開を行わずにデフォルト文脈プリセットの生データのみをマージする。
    fn merge_default_presets_raw(&mut self) -> anyhow::Result<()> {
        let preset = Self::default_preset_data()?;
        for (ctx, cands) in preset.context_frequencies {
            let entry = self.context_frequencies.entry(ctx).or_default();
            for (cand, count) in cands {
                let current = entry.entry(cand).or_insert(0);
                *current = (*current).max(count);
            }
        }
        Ok(())
    }

    /// 組み込みのデフォルト文脈プリセット（context_frequencies）をマージする。
    /// 既存の frequencies や context_frequencies は維持され、プリセット側の値が追加・更新（最大値）される。
    pub fn merge_default_presets(&mut self) -> anyhow::Result<()> {
        self.merge_default_presets_raw()?;
        self.expand_aliases();
        Ok(())
    }

    /// context_frequencies が空の場合（seed ファイルが存在しない・文脈データ未登録など）に、
    /// 組み込みプリセットをメモリ上で適用する（ファイルには書き込まない）。
    /// 適用した場合は true を返す。
    pub fn ensure_default_context_presets(&mut self) -> bool {
        if !self.context_frequencies.is_empty() {
            return false;
        }
        self.merge_default_presets().is_ok()
    }

    /// seed ファイル（~/.ruskkserv-frequency.json）を初期化またはマージして保存する。
    /// force が true の場合はプリセットで完全上書きし、false の場合は既存データを保持したままマージする。
    pub fn init_seed(&mut self, force: bool) -> anyhow::Result<()> {
        if force {
            let preset = Self::default_preset_data()?;
            self.frequencies = preset.frequencies;
            self.context_frequencies = preset.context_frequencies;
        } else {
            self.reload_raw_if_exists()?;
            self.merge_default_presets_raw()?;
        }
        self.save()?;
        self.expand_aliases();
        Ok(())
    }

    /// macSKK などのユーザー辞書 (skk-jisyo.utf8) をパースし、
    /// 候補の順番に応じたスコアを frequencies にインポート（最大値で冪等更新）する。
    /// 送りありブロック（例: `きr /[る/着/切/伐/]/[り/切/]/`）も正確にパースし、
    /// `"[る"` や `"]"` などの不要記号の混入を完全に防止する。
    /// context_frequencies が空の場合はデフォルトプリセットも自動注入する。
    pub fn import_from_skk_dict<P: AsRef<Path>>(&mut self, dict_path: P) -> anyhow::Result<()> {
        let content = fs::read_to_string(dict_path)?;
        self.reload_raw_if_exists()?;

        for line in content.lines() {
            let line = line.trim();
            // コメント行や空行はスキップ
            if line.is_empty() || line.starts_with(';') {
                continue;
            }

            // フォーマット: 見出し語 /候補1/候補2/ または 見出し語 /[送り/候補1/]/
            let mut parts = line.splitn(2, ' ');
            let midashi = match parts.next() {
                Some(m) if !m.is_empty() => m,
                _ => continue,
            };

            let cands_part = match parts.next() {
                Some(c) if c.starts_with('/') && c.ends_with('/') => c,
                _ => continue,
            };

            let candidates = parse_dict_line_candidates(cands_part);
            if candidates.is_empty() {
                continue;
            }

            let entry = self.frequencies.entry(midashi.to_string()).or_default();

            // 順番に応じてスコアを付与 (先頭ほど高い)
            // 候補がN個の場合、1番目=N点, 2番目=N-1点, ..., N番目=1点
            // 定期インポート（毎時）でスコアが際限なく膨らみ文脈スコアを上回らないよう、
            // 加算ではなく最大値で更新する（冪等）。ユーザー辞書から消えたエントリのスコアは保持される。
            let total_cands = candidates.len() as u64;
            for (idx, cand_text) in candidates.iter().enumerate() {
                let score = total_cands.saturating_sub(idx as u64);
                let current = entry.entry(cand_text.clone()).or_insert(0);
                *current = (*current).max(score);
            }
        }

        // context_frequencies が空なら、デフォルトプリセットも一緒に初期化
        if self.context_frequencies.is_empty() {
            let _ = self.merge_default_presets_raw();
        }

        // エイリアス展開前のクリーンなデータを保存し、その後メモリ上でエイリアスを展開する
        self.save()?;
        self.expand_aliases();

        Ok(())
    }

    /// context_frequencies から文脈マップを取得する。
    /// expand_aliases() によりロード時にエイリアスが展開済みのため、O(1) の HashMap::get のみで完結する。
    fn get_context_map<'a>(&'a self, ctx: &str) -> Option<&'a HashMap<String, u64>> {
        self.context_frequencies.get(ctx)
    }

    /// frequencies から見出し語マップを取得する。
    /// ASCII 大文字を含む場合（例: "てだR"）は小文字化（"てだr"）でも検索を試行する。
    fn get_freq_map<'a>(&'a self, midashi: &str) -> Option<&'a HashMap<String, u64>> {
        if let Some(map) = self.frequencies.get(midashi) {
            return Some(map);
        }
        if midashi.bytes().any(|b| b.is_ascii_uppercase()) {
            let lower = midashi.to_ascii_lowercase();
            return self.frequencies.get(&lower);
        }
        None
    }

    /// 見出し語または現在の文脈に対して、並び替えルールが存在するかを超高速判定する。
    /// これが false の場合、候補のパースや並び替え処理を一切行わずに直結バイパス（完全ゼロコピー）できる。
    pub fn should_rank<S: AsRef<str>>(&self, context: &[S], midashi: &str) -> bool {
        if self
            .get_freq_map(midashi)
            .map(|m| !m.is_empty())
            .unwrap_or(false)
        {
            return true;
        }
        if !context.is_empty() && !self.context_frequencies.is_empty() {
            for ctx in context {
                if self.get_context_map(ctx.as_ref()).is_some() {
                    return true;
                }
            }
        }
        false
    }

    /// 見出し語に対する frequencies の最高スコア候補が、現在の候補群に含まれていないか判定する。
    /// Primary (azooKey) に存在せず Fallback (yaskkserv2) 側にのみ存在するユーザー辞書語（例: "みぎや" -> "→"）を
    /// 補完マージすべきかどうかの判定に使用する。
    pub fn is_top_freq_candidate_missing(&self, midashi: &str, candidates: &[&str]) -> bool {
        let Some(freq_map) = self.get_freq_map(midashi) else {
            return false;
        };
        let Some(&max_score) = freq_map.values().max() else {
            return false;
        };
        if max_score == 0 {
            return false;
        }
        let best_in_cands = candidates
            .iter()
            .map(|&c| get_score(freq_map, clean_candidate(c)))
            .max()
            .unwrap_or(0);
        best_in_cands < max_score
    }

    /// 借用スライス (&str) を対象に、アロケーションなしで候補を並び替える。
    /// annotation_priority が true の場合、macOS システム辞書（大辞林等）の掲載語
    /// または SKK 注釈付き単語にボーナススコア（100点）を加算し、さらに送りなし見出しでは
    /// 送り仮名・助詞が混入していない純粋な送りなし候補に形状ボーナス（20点）を加算して総合順位を決定する。
    pub fn rank_candidates_borrowed<'a, S: AsRef<str>>(
        &self,
        context: &[S],
        midashi: &str,
        candidates: &[&'a str],
        annotation_priority: bool,
    ) -> Vec<&'a str> {
        self.rank_candidates_borrowed_with_deadline(
            context,
            midashi,
            candidates,
            annotation_priority,
            None,
        )
        .0
    }

    /// `ffi_deadline` を上限として未キャッシュ時のシステム辞書 FFI 照会を制御しながら候補を並び替える。
    /// 予算超過により未照会のままスキップされた上位候補（`idx < 12`）のスライスも併せて返す。
    pub fn rank_candidates_borrowed_with_deadline<'a, S: AsRef<str>>(
        &self,
        context: &[S],
        midashi: &str,
        candidates: &[&'a str],
        annotation_priority: bool,
        ffi_deadline: Option<std::time::Instant>,
    ) -> (Vec<&'a str>, Vec<&'a str>) {
        if candidates.len() <= 1 {
            return (candidates.to_vec(), Vec::new());
        }

        let freq_map = self.get_freq_map(midashi);
        let has_freq = freq_map.map(|m| !m.is_empty()).unwrap_or(false);

        // 文脈マップの取得: RuSKKserv の文脈は通常直前の確定単語1語（要素数 0 または 1）
        // 大半のケースで中間 Vec を作らずゼロアロケーションで処理する
        let single_ctx_map = if context.len() == 1 && !self.context_frequencies.is_empty() {
            self.get_context_map(context[0].as_ref())
        } else {
            None
        };
        let multi_ctx_maps: Vec<&HashMap<String, u64>> =
            if context.len() > 1 && !self.context_frequencies.is_empty() {
                context
                    .iter()
                    .filter_map(|ctx| self.get_context_map(ctx.as_ref()))
                    .collect()
            } else {
                Vec::new()
            };

        let has_context = single_ctx_map.is_some() || !multi_ctx_maps.is_empty();
        let is_affix = !matches!(
            crate::okuri::classify_midashi(midashi),
            crate::okuri::MidashiKind::Normal(_)
        );
        let effective_annotation_priority = annotation_priority && !is_affix;
        if !has_freq && !has_context && !effective_annotation_priority {
            return (candidates.to_vec(), Vec::new());
        }

        const ANNOTATION_BONUS: u64 = 100;
        const OKURI_NASHI_CLEAN_BONUS: u64 = 20;

        let parsed_okuri = if effective_annotation_priority {
            crate::okuri::parse_okuri_midashi_extended(midashi)
        } else {
            None
        };
        let is_okuri = parsed_okuri.is_some();

        let mut skipped_cold: Vec<&'a str> = Vec::new();
        let mut indexed_cands: Vec<(usize, &'a str, u64)> = Vec::with_capacity(candidates.len());

        for (idx, &cand) in candidates.iter().enumerate() {
            let clean = clean_candidate(cand);
            let global_count = freq_map.map(|m| get_score(m, clean)).unwrap_or(0);
            let context_score: u64 = if let Some(m) = single_ctx_map {
                get_score(m, clean)
            } else if !multi_ctx_maps.is_empty() {
                multi_ctx_maps.iter().map(|m| get_score(m, clean)).sum()
            } else {
                0
            };

            let annotation_score: u64 = if effective_annotation_priority {
                // 上位12件かつ ffi_deadline 内のみ未キャッシュ時のシステム辞書 FFI 照会を許可し、
                // 13件目以降または予算超過時はキャッシュヒットのみ採用して応答タイムアウトを防止する
                let within_index_cap = idx < 12;
                let (in_dict, cold_skipped) =
                    crate::system_dict::is_annotated_or_in_dict_parsed_budgeted(
                        cand,
                        parsed_okuri.as_ref(),
                        within_index_cap,
                        ffi_deadline,
                    );
                if within_index_cap && cold_skipped {
                    skipped_cold.push(cand);
                }
                let dict_bonus = if in_dict { ANNOTATION_BONUS } else { 0 };
                let shape_bonus = if !is_okuri
                    && (in_dict || !is_unverified_okuri_fragment(clean))
                {
                    OKURI_NASHI_CLEAN_BONUS
                } else {
                    0
                };
                dict_bonus + shape_bonus
            } else {
                0
            };

            let total_score = annotation_score
                .saturating_add(global_count)
                .saturating_add(context_score.saturating_mul(10));
            indexed_cands.push((idx, cand, total_score));
        }

        if indexed_cands.iter().all(|(_, _, score)| *score == 0) {
            return (candidates.to_vec(), skipped_cold);
        }

        indexed_cands.sort_unstable_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));

        (
            indexed_cands.into_iter().map(|(_, cand, _)| cand).collect(),
            skipped_cold,
        )
    }

    /// seed ロード時に frequencies / context_frequencies の各マップに対して、
    /// 送りあり変換の語幹と終止形を繋ぐエイリアス（「切 ↔ 切る」「確認 ↔ 確認する」等）のみを展開する。
    /// 漢字熟語やカタカナ語の途中を切るような接頭辞部分文字列（例: 「漁港 → 漁」「時間 → 時」「包丁 → 包」）は
    /// 通常の変換と接頭語が混同される原因となるため一切生成しない。
    /// これによりランタイムのスコア照合が HashMap::get の O(1) のみで完結する。
    pub fn expand_aliases(&mut self) {
        expand_map_aliases(&mut self.frequencies);
        expand_context_map_aliases(&mut self.context_frequencies);
    }
}

/// マップからスコアを取得する。
/// expand_aliases() によりロード時にエイリアスが展開済みのため、O(1) の HashMap::get のみで完結する。
#[inline]
fn get_score(map: &HashMap<String, u64>, target: &str) -> u64 {
    map.get(target).copied().unwrap_or(0)
}

/// 文字が平仮名（`ぁ`..=`ん`）か判定する。
#[inline]
fn is_hiragana_char(c: char) -> bool {
    ('\u{3041}'..='\u{3096}').contains(&c)
}

/// 送り仮名付きの単語（末尾が平仮名で、かつ非平仮名の語幹を含む語、例: "切る", "確認する", "受け入れる"）から、
/// SKK の送りあり変換で用いられる語幹エイリアス（平仮名の直前で切り出したプレフィックス、例: "切", "確認", "受け入れ", "受"）を抽出する。
/// 純粋な漢字熟語やカタカナ語（例: "漁港", "時間", "包丁", "ヘリコプター"）からは一切プレフィックスを生成しない。
fn okuri_stem_aliases(word: &str) -> Vec<&str> {
    let mut chars = word.chars();
    let Some(last_char) = chars.next_back() else {
        return Vec::new();
    };
    if !is_hiragana_char(last_char) {
        return Vec::new();
    }
    if !word.chars().any(|c| !is_hiragana_char(c)) {
        return Vec::new();
    }

    let mut stems = Vec::new();
    for (idx, ch) in word.char_indices().skip(1) {
        // 平仮名が始まる位置でのみ切り出すことで、漢字連続（"確認" → "確" 等）の途中切断を完全に防ぐ
        if is_hiragana_char(ch) {
            let prefix = &word[..idx];
            if prefix.chars().any(|c| !is_hiragana_char(c)) {
                stems.push(prefix);
            }
        }
    }
    stems
}

/// frequencies マップの内部マップに送りあり語幹エイリアスを展開する。
/// 例: {"切る": 3} に対して "切" をキーとして同スコアを追加（既存値がある場合は大きい方を採用）。
fn expand_map_aliases(map: &mut HashMap<String, HashMap<String, u64>>) {
    for inner in map.values_mut() {
        expand_inner_aliases(inner);
    }
}

/// context_frequencies マップのキー側とバリュー側の両方に送りあり語幹エイリアスを展開する。
/// キー側: "切る" があれば "切" でも同じマップを参照できるようにエントリを追加（漢字熟語 "時間" → "時" などは展開しない）。
/// バリュー側: 各内部マップの値にも送りあり語幹エイリアスを展開。
fn expand_context_map_aliases(ctx_map: &mut HashMap<String, HashMap<String, u64>>) {
    // 1. バリュー側のエイリアス展開
    for inner in ctx_map.values_mut() {
        expand_inner_aliases(inner);
    }

    // 2. キー側のエイリアス展開: "切る" があれば語幹 "切" でも参照できるようにする
    let mut aliases: Vec<(String, HashMap<String, u64>)> = Vec::new();
    for (k, v) in ctx_map.iter() {
        for stem in okuri_stem_aliases(k) {
            if !ctx_map.contains_key(stem) {
                aliases.push((stem.to_string(), v.clone()));
            }
        }
    }

    for (alias_key, alias_map) in aliases {
        let entry = ctx_map.entry(alias_key).or_default();
        for (cand, score) in alias_map {
            let current = entry.entry(cand).or_insert(0);
            *current = (*current).max(score);
        }
    }
}

/// 内部マップ（候補 → スコア）に対して送りあり語幹エイリアスを双方向に展開する。
/// 例: {"切る": 3} ↔ {"切": 3}（既存値がある場合は大きい方を採用）。
fn expand_inner_aliases(inner: &mut HashMap<String, u64>) {
    let mut aliases = Vec::new();
    for (k, &score) in inner.iter() {
        for stem in okuri_stem_aliases(k) {
            let stem_score = inner.get(stem).copied().unwrap_or(0);
            let merged_score = score.max(stem_score);
            aliases.push((stem.to_string(), merged_score));
            if merged_score > score {
                aliases.push((k.clone(), merged_score));
            }
        }
    }

    for (alias_key, score) in aliases {
        let current = inner.entry(alias_key).or_insert(0);
        *current = (*current).max(score);
    }
}

/// 候補文字列から注釈（`;` 以降）を除去した本体文字列を返す。
pub fn clean_candidate(cand: &str) -> &str {
    match cand.split_once(';') {
        Some((word, _)) => word.trim(),
        None => cand.trim(),
    }
}

/// 送りなし見出しに対する候補として、辞書未掲載時に「漢字/カタカナ＋平仮名（送り仮名・助詞漏れ・交ぜ書き断片）」とみなすか判定する。
/// 例: "貸そ", "右や", "左や", "上や", "解そ", "したヤ", "書こ", "点けん" などは true。
/// 純粋な漢字・カタカナ・記号（"画素", "右矢", "下谷", "→"）や、敬語接頭辞＋漢字（"お肉"）は false。
pub fn is_unverified_okuri_fragment(cand_clean: &str) -> bool {
    let s = if let Some(rest) = cand_clean
        .strip_prefix('お')
        .or_else(|| cand_clean.strip_prefix('ご'))
    {
        if rest.chars().next().is_some_and(crate::okuri::is_kanji) {
            rest
        } else {
            cand_clean
        }
    } else {
        cand_clean
    };

    let mut has_hiragana = false;
    let mut has_kanji_or_katakana = false;
    for c in s.chars() {
        if ('\u{3041}'..='\u{3096}').contains(&c) {
            has_hiragana = true;
        } else if crate::okuri::is_kanji(c)
            || ('\u{30A1}'..='\u{30F6}').contains(&c)
            || c == 'ー'
        {
            has_kanji_or_katakana = true;
        }
        if has_hiragana && has_kanji_or_katakana {
            return true;
        }
    }
    false
}

/// SKK 辞書エントリの候補部分（`/[る/着/切/伐/]/[り/切/]/` や `/変換/返還/`）から、
/// 送り仮名ブロック `[...]` や注釈 `;...` を解析し、純粋な候補文字列のリストを返す。
pub fn parse_dict_line_candidates(cands_part: &str) -> Vec<String> {
    let mut candidates: Vec<&str> = Vec::new();
    let mut seen: Option<HashSet<&str>> = None;
    let trimmed = cands_part.trim_matches('/');
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut in_bracket = false;
    let mut is_first_in_bracket = false;

    for part in trimmed.split('/') {
        let mut p = part;
        if p.starts_with('[') {
            in_bracket = true;
            is_first_in_bracket = true;
            p = &p[1..];
        }

        let ends_bracket = p.ends_with(']');
        if ends_bracket {
            p = &p[..p.len() - 1];
        }

        if in_bracket && is_first_in_bracket {
            // 送り仮名ブロックの最初の要素は「送り仮名」（例: "る", "り"）なので候補ではない
            is_first_in_bracket = false;
        } else if !p.is_empty() {
            let cand_text = clean_candidate(p);
            crate::encoding::push_unique_str(&mut candidates, &mut seen, cand_text);
        }

        if ends_bracket {
            in_bracket = false;
        }
    }

    candidates.into_iter().map(|s| s.to_string()).collect()
}

pub type SharedPredictor = Arc<RwLock<FrequencyPredictor>>;

/// テスト専用: 所有権版の rank_candidates
#[cfg(test)]
impl FrequencyPredictor {
    pub fn rank_candidates<S: AsRef<str>>(
        &self,
        context: &[S],
        midashi: &str,
        candidates: &[String],
    ) -> Vec<String> {
        self.rank_candidates_with_annotation(context, midashi, candidates, false)
    }

    pub fn rank_candidates_with_annotation<S: AsRef<str>>(
        &self,
        context: &[S],
        midashi: &str,
        candidates: &[String],
        annotation_priority: bool,
    ) -> Vec<String> {
        let borrowed: Vec<&str> = candidates.iter().map(|s| s.as_str()).collect();
        let ranked =
            self.rank_candidates_borrowed(context, midashi, &borrowed, annotation_priority);
        ranked.into_iter().map(|s| s.to_string()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predictor_ranking_by_frequency() {
        let mut predictor = FrequencyPredictor::new(None);
        let candidates = vec!["愛".to_string(), "相".to_string(), "藍".to_string()];

        // 初期状態ではスコアがないため元の順序を維持
        let ranked = predictor.rank_candidates(&[] as &[String], "あい", &candidates);
        assert_eq!(ranked, vec!["愛", "相", "藍"]);

        // seed データを直接設定（手動編集を模倣）
        predictor.frequencies.insert("あい".to_string(), {
            let mut m = HashMap::new();
            m.insert("相".to_string(), 2);
            m.insert("藍".to_string(), 7);
            m
        });
        predictor.expand_aliases();

        let ranked_after = predictor.rank_candidates(&[] as &[String], "あい", &candidates);
        assert_eq!(ranked_after, vec!["藍", "相", "愛"]);
    }

    #[test]
    fn kanji_context_cooccurrence_ranking() {
        let mut predictor = FrequencyPredictor::new(None);
        let candidates = vec!["着る".to_string(), "切る".to_string(), "伐る".to_string()];
        let stem_candidates = vec!["着".to_string(), "切".to_string(), "伐".to_string()];

        // seed データを直接設定（手動編集を模倣）
        predictor.context_frequencies.insert("肉".to_string(), {
            let mut m = HashMap::new();
            m.insert("切る".to_string(), 3);
            m
        });
        predictor.context_frequencies.insert("服".to_string(), {
            let mut m = HashMap::new();
            m.insert("着る".to_string(), 2);
            m
        });
        predictor.context_frequencies.insert("木".to_string(), {
            let mut m = HashMap::new();
            m.insert("伐る".to_string(), 4);
            m
        });
        predictor.expand_aliases();

        // 1. 文脈「肉」のとき、「切る」（および語幹「切」）が第1候補
        let ranked_niku = predictor.rank_candidates(&["肉".to_string()], "きる", &candidates);
        assert_eq!(ranked_niku[0], "切る");
        let ranked_niku_stem =
            predictor.rank_candidates(&["肉".to_string()], "きr", &stem_candidates);
        assert_eq!(ranked_niku_stem[0], "切");

        // 2. 文脈「服」のとき、「着る」（および語幹「着」）が第1候補
        let ranked_fuku = predictor.rank_candidates(&["服".to_string()], "きる", &candidates);
        assert_eq!(ranked_fuku[0], "着る");
        let ranked_fuku_stem =
            predictor.rank_candidates(&["服".to_string()], "きr", &stem_candidates);
        assert_eq!(ranked_fuku_stem[0], "着");

        // 3. 文脈「木」のとき、「伐る」（および語幹「伐」）が第1候補
        let ranked_ki = predictor.rank_candidates(&["木".to_string()], "きる", &candidates);
        assert_eq!(ranked_ki[0], "伐る");
        let ranked_ki_stem =
            predictor.rank_candidates(&["木".to_string()], "きr", &stem_candidates);
        assert_eq!(ranked_ki_stem[0], "伐");

        // 4. 文脈キー側の柔軟照合: 辞書キーが「切る」でも、文脈が語幹「切」でヒットすること
        predictor.context_frequencies.insert("切る".to_string(), {
            let mut m = HashMap::new();
            m.insert("包丁".to_string(), 10);
            m
        });
        predictor.expand_aliases();
        let nouchi_cands = vec!["包丁".to_string(), "庖丁".to_string()];
        let ranked_hocho =
            predictor.rank_candidates(&["切".to_string()], "ほうちょう", &nouchi_cands);
        assert_eq!(ranked_hocho[0], "包丁");
    }

    #[test]
    fn test_import_from_skk_dict() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        // ダミーの SKK 辞書ファイルを作成（macSKK の送りありブロック形式を含む）
        let mut dict_file = NamedTempFile::new().unwrap();
        writeln!(dict_file, ";; okuri-ari entries.").unwrap();
        writeln!(dict_file, "きr /[る/着/切;注釈/伐/]/[り/切/]/").unwrap();
        writeln!(dict_file, ";; okuri-nasi entries.").unwrap();
        writeln!(dict_file, "ふく /服/吹く/副/").unwrap();

        // テスト用の JSON 保存先
        let json_file = NamedTempFile::new().unwrap();

        let mut predictor = FrequencyPredictor::new(Some(json_file.path().to_path_buf()));
        // 既存の context_frequencies が保持されることを確認するため追加
        predictor.context_frequencies.insert("肉".to_string(), {
            let mut m = HashMap::new();
            m.insert("切".to_string(), 10);
            m
        });

        // インポート実行
        predictor.import_from_skk_dict(dict_file.path()).unwrap();

        // frequencies が正しくインポートされているか確認
        // 送りブロック `/[る/着/切;注釈/伐/]/[り/切/]/` から "着"=3, "切"=2, "伐"=1 となること
        // "[る" や "]" などのゴミが一切含まれないこと
        let kir_freqs = predictor.frequencies.get("きr").unwrap();
        assert!(!kir_freqs.contains_key("[る"));
        assert!(!kir_freqs.contains_key("]"));
        assert_eq!(kir_freqs.get("着").copied().unwrap_or(0), 3);
        assert_eq!(kir_freqs.get("切").copied().unwrap_or(0), 2);
        assert_eq!(kir_freqs.get("伐").copied().unwrap_or(0), 1);

        // 注釈付き候補（例: "切;注釈あり"）でもスコア照合できていること
        let ranked = predictor.rank_candidates(
            &[] as &[String],
            "きr",
            &[
                "伐".to_string(),
                "切;注釈あり".to_string(),
                "着".to_string(),
            ],
        );
        assert_eq!(ranked[0], "着");
        assert_eq!(ranked[1], "切;注釈あり");
        assert_eq!(ranked[2], "伐");

        // "ふく": 服=3, 吹く=2, 副=1
        let fuku_freqs = predictor.frequencies.get("ふく").unwrap();
        assert_eq!(fuku_freqs.get("服").copied().unwrap_or(0), 3);
        assert_eq!(fuku_freqs.get("吹く").copied().unwrap_or(0), 2);
        assert_eq!(fuku_freqs.get("副").copied().unwrap_or(0), 1);

        // context_frequencies が維持されているか確認
        let niku_ctx = predictor.context_frequencies.get("肉").unwrap();
        assert_eq!(niku_ctx.get("切").copied().unwrap_or(0), 10);

        // ファイルにも保存されているか確認
        let mut loaded = FrequencyPredictor::new(Some(json_file.path().to_path_buf()));
        loaded.load(json_file.path()).unwrap();
        assert_eq!(
            loaded
                .frequencies
                .get("きr")
                .unwrap()
                .get("着")
                .copied()
                .unwrap_or(0),
            3
        );
    }

    #[test]
    fn test_init_seed_and_merge_default_presets() {
        use tempfile::NamedTempFile;

        let json_file = NamedTempFile::new().unwrap();
        let mut predictor = FrequencyPredictor::new(Some(json_file.path().to_path_buf()));

        // 初期状態では context_frequencies は空
        assert!(predictor.context_frequencies.is_empty());

        // init_seed を実行
        predictor.init_seed(false).unwrap();

        // デフォルトプリセットの代表的ペアが読み込まれていること
        assert_eq!(
            predictor
                .context_frequencies
                .get("服")
                .unwrap()
                .get("着る")
                .copied()
                .unwrap_or(0),
            10
        );
        assert_eq!(
            predictor
                .context_frequencies
                .get("肉")
                .unwrap()
                .get("切る")
                .copied()
                .unwrap_or(0),
            10
        );
        assert_eq!(
            predictor
                .context_frequencies
                .get("時間")
                .unwrap()
                .get("計る")
                .copied()
                .unwrap_or(0),
            10
        );

        // 保存された JSON ファイルには中間プレフィックス（"プ" 等）が混入していないこと
        let raw_saved: FrequencyPredictor =
            serde_json::from_str(&fs::read_to_string(json_file.path()).unwrap()).unwrap();
        assert!(!raw_saved.context_frequencies.contains_key("プ"));

        // ファイルから再ロードしても正しく永続化・メモリ上エイリアス展開されていること
        let reloaded = FrequencyPredictor::new(Some(json_file.path().to_path_buf()));
        assert_eq!(
            reloaded
                .context_frequencies
                .get("服")
                .unwrap()
                .get("着る")
                .copied()
                .unwrap_or(0),
            10
        );
        assert_eq!(
            reloaded
                .context_frequencies
                .get("服")
                .unwrap()
                .get("着")
                .copied()
                .unwrap_or(0),
            10
        );
    }

    #[test]
    fn test_corrupted_json_protects_against_overwrite() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut json_file = NamedTempFile::new().unwrap();
        // 壊れた JSON（構文エラー）を書き込む
        json_file
            .write_all(b"{ \"frequencies\": { broken json ...")
            .unwrap();

        let mut predictor = FrequencyPredictor::new(Some(json_file.path().to_path_buf()));
        // 構文エラーのファイルが存在する場合、init_seed(false) はエラーを返し、上書き保存しないこと
        assert!(predictor.init_seed(false).is_err());

        // ダミーの SKK 辞書
        let mut dict_file = NamedTempFile::new().unwrap();
        writeln!(dict_file, "ふく /服/").unwrap();

        // import_from_skk_dict もエラーを返し、上書き保存しないこと
        assert!(predictor.import_from_skk_dict(dict_file.path()).is_err());

        // 元の壊れたファイル内容が保持されていること（空やデフォルトで上書きされていないこと）
        let content = fs::read_to_string(json_file.path()).unwrap();
        assert!(content.contains("broken json"));
    }

    #[test]
    fn test_should_rank_fast_path() {
        let mut predictor = FrequencyPredictor::new(None);
        // ルールが何もない場合は false
        assert!(!predictor.should_rank(&[] as &[String], "とうきょう"));
        assert!(!predictor.should_rank(&["服".to_string()], "とうきょう"));

        // 文脈ルールがある場合は true
        predictor.context_frequencies.insert("服".to_string(), {
            let mut m = HashMap::new();
            m.insert("着る".to_string(), 10);
            m
        });
        predictor.expand_aliases();
        assert!(predictor.should_rank(&["服".to_string()], "きr"));
        // 文脈と一致しない場合は false
        assert!(!predictor.should_rank(&["車".to_string()], "きr"));

        // 単語頻度ルールがある場合は true
        predictor.frequencies.insert("あい".to_string(), {
            let mut m = HashMap::new();
            m.insert("愛".to_string(), 5);
            m
        });
        predictor.expand_aliases();
        assert!(predictor.should_rank(&[] as &[String], "あい"));
    }

    #[test]
    fn test_tedare_ranking() {
        let mut predictor = FrequencyPredictor::new(None);
        predictor.frequencies.insert("てだr".to_string(), {
            let mut m = HashMap::new();
            m.insert("手練".to_string(), 2);
            m.insert("手".to_string(), 2);
            m
        });
        predictor.expand_aliases();
        let candidates = vec!["手だ", "手練", "て誰", "手足"];

        // 小文字 "てだr" での並び替え
        assert!(predictor.should_rank(&[] as &[String], "てだr"));
        let ranked =
            predictor.rank_candidates_borrowed(&[] as &[&str], "てだr", &candidates, false);
        assert_eq!(ranked[0], "手練");

        // 大文字 "てだR" でも小文字正規化によりヒットすること
        assert!(predictor.should_rank(&[] as &[String], "てだR"));
        let ranked_upper =
            predictor.rank_candidates_borrowed(&[] as &[&str], "てだR", &candidates, false);
        assert_eq!(ranked_upper[0], "手練");
    }

    #[test]
    fn test_annotation_priority_scoring() {
        let predictor = FrequencyPredictor::new(None);
        // 注釈付き候補、大辞林にある語、辞書にない造語
        let candidates = vec![
            "謎造語xyz".to_string(),
            "時間;time".to_string(), // 注釈付き (+100)
            "東京".to_string(),      // システム辞書掲載語 (+100)
        ];

        // annotation_priority = true の場合、注釈・辞書語が優先昇格する
        let ranked = predictor.rank_candidates_with_annotation(
            &[] as &[String],
            "とうきょう",
            &candidates,
            true,
        );
        assert_ne!(ranked[0], "謎造語xyz");
        assert_eq!(ranked[2], "謎造語xyz"); // 辞書外の造語が最下位に沈む

        // annotation_priority = false の場合、元の順序が維持される
        let unranked = predictor.rank_candidates_with_annotation(
            &[] as &[String],
            "とうきょう",
            &candidates,
            false,
        );
        assert_eq!(unranked[0], "謎造語xyz");
    }

    #[test]
    fn test_okuri_nashi_prioritizes_clean_candidates_over_fragments() {
        let mut predictor = FrequencyPredictor::new(None);

        // 1. 「がそ」: azooKey が "貸そ"（動詞未然・意志形断片）を先頭に返しても、"画素" が第1候補に昇格する
        let gaso_cands = vec!["貸そ".to_string(), "画素".to_string()];
        let ranked_gaso =
            predictor.rank_candidates_with_annotation(&[] as &[String], "がそ", &gaso_cands, true);
        assert_eq!(ranked_gaso, vec!["画素", "貸そ"]);

        // 2. 「みぎや」: 両方とも大辞林未掲載でも、送り仮名・助詞混じり断片 "右や" より純粋な漢字・記号 ("→", "右矢") が優先される
        predictor.frequencies.insert("みぎや".to_string(), {
            let mut m = HashMap::new();
            m.insert("→".to_string(), 1);
            m
        });
        predictor.expand_aliases();
        assert!(predictor.is_top_freq_candidate_missing("みぎや", &["右矢", "右や"]));
        assert!(!predictor.is_top_freq_candidate_missing("みぎや", &["右矢", "右や", "→"]));

        let migiya_cands = vec!["右や".to_string(), "右矢".to_string(), "→".to_string()];
        let ranked_migiya = predictor.rank_candidates_with_annotation(
            &[] as &[String],
            "みぎや",
            &migiya_cands,
            true,
        );
        assert_eq!(ranked_migiya, vec!["→", "右矢", "右や"]);

        // 3. is_unverified_okuri_fragment 単体検証
        assert!(is_unverified_okuri_fragment("貸そ"));
        assert!(is_unverified_okuri_fragment("右や"));
        assert!(is_unverified_okuri_fragment("したヤ"));
        assert!(!is_unverified_okuri_fragment("画素"));
        assert!(!is_unverified_okuri_fragment("右矢"));
        assert!(!is_unverified_okuri_fragment("→"));
        assert!(!is_unverified_okuri_fragment("お肉"));
    }

    #[test]
    fn test_expand_aliases_only_strips_trailing_okurigana_not_noun_prefixes() {
        let mut predictor = FrequencyPredictor::new(None);
        predictor.frequencies.insert("ぎょこう".to_string(), {
            let mut m = HashMap::new();
            m.insert("漁港".to_string(), 1);
            m
        });
        predictor.frequencies.insert("よみこm".to_string(), {
            let mut m = HashMap::new();
            m.insert("読み込".to_string(), 1);
            m
        });
        predictor.context_frequencies.insert("時間".to_string(), {
            let mut m = HashMap::new();
            m.insert("割く".to_string(), 10);
            m
        });
        predictor
            .context_frequencies
            .insert("ヘリコプター".to_string(), {
                let mut m = HashMap::new();
                m.insert("飛ばす".to_string(), 10);
                m
            });
        predictor
            .context_frequencies
            .insert("確認する".to_string(), {
                let mut m = HashMap::new();
                m.insert("事項".to_string(), 10);
                m
            });

        predictor.expand_aliases();

        // 1. 漢字・カタカナ名詞の先頭部分文字列（"漁", "読", "読み", "時", "ヘリコプタ"）が生成されないこと
        let gyokou_map = predictor.frequencies.get("ぎょこう").unwrap();
        assert_eq!(gyokou_map.get("漁港"), Some(&1));
        assert!(!gyokou_map.contains_key("漁"));

        let yomikom_map = predictor.frequencies.get("よみこm").unwrap();
        assert_eq!(yomikom_map.get("読み込"), Some(&1));
        assert!(!yomikom_map.contains_key("読"));
        assert!(!yomikom_map.contains_key("読み"));

        assert!(!predictor.context_frequencies.contains_key("時"));
        assert!(!predictor.context_frequencies.contains_key("ヘリコプタ"));

        // 2. 送り仮名付きの動詞・サ変動詞（"割く" -> "割", "飛ばす" -> "飛ば"/"飛", "確認する" -> "確認"）は展開されること
        let jikan_map = predictor.context_frequencies.get("時間").unwrap();
        assert_eq!(jikan_map.get("割く"), Some(&10));
        assert_eq!(jikan_map.get("割"), Some(&10));

        let heli_map = predictor.context_frequencies.get("ヘリコプター").unwrap();
        assert_eq!(heli_map.get("飛ばす"), Some(&10));
        assert_eq!(heli_map.get("飛ば"), Some(&10));
        assert_eq!(heli_map.get("飛"), Some(&10));

        let kakunin_stem_map = predictor.context_frequencies.get("確認").unwrap();
        assert_eq!(kakunin_stem_map.get("事項"), Some(&10));
    }

    #[test]
    fn test_affix_midashi_preserves_skk_order_without_annotation_hijack() {
        let predictor = FrequencyPredictor::new(None);
        // SKK-JISYO.L の `さい> /再/最/歳;?/` や `ひ> /被/非/乾;.../` で、
        // 注釈付きの末尾候補（`歳;?`, `乾;...`）が主要接頭語（`再`, `被`）を追い越さないこと
        let sai_prefix = ["再", "最", "歳;?"];
        let ranked_sai =
            predictor.rank_candidates_borrowed(&[] as &[&str], "さい>", &sai_prefix, true);
        assert_eq!(ranked_sai, vec!["再", "最", "歳;?"]);

        let hi_prefix = ["被", "非", "乾;かわく"];
        let ranked_hi =
            predictor.rank_candidates_borrowed(&[] as &[&str], "ひ>", &hi_prefix, true);
        assert_eq!(ranked_hi, vec!["被", "非", "乾;かわく"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_rank_candidates_with_expired_deadline_skips_cold_ffi_and_reports() {
        use std::time::{Duration, Instant};

        let predictor = FrequencyPredictor::new(None);
        // 「時間」だけ事前にシステム辞書キャッシュへ載せておく
        assert!(crate::system_dict::is_annotated_or_in_dict("時間", None));

        let expired = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        let cold_a = "予算超過未キャッシュテスト語A999111";
        let cold_b = "予算超過未キャッシュテスト語B999222";
        let cands = [cold_a, "時間", cold_b];

        let (ranked, skipped) = predictor.rank_candidates_borrowed_with_deadline(
            &[] as &[&str],
            "じかん",
            &cands,
            true,
            Some(expired),
        );

        // キャッシュ済みの「時間」は加点されて第1候補に昇格する
        assert_eq!(ranked.first().copied(), Some("時間"));
        // 未キャッシュの2語はコールド FFI をスキップしたことが報告される
        assert!(skipped.contains(&cold_a));
        assert!(skipped.contains(&cold_b));
        assert!(!skipped.contains(&"時間"));
    }

    #[test]
    fn test_atomic_save_permissions_and_saturating_score_overflow() {
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("freq.json");

        let mut predictor = FrequencyPredictor::new_unloaded(Some(path.clone()));
        predictor.frequencies.insert("てすと".to_string(), {
            let mut m = HashMap::new();
            m.insert("テスト".to_string(), u64::MAX);
            m
        });
        predictor.context_frequencies.insert("文脈".to_string(), {
            let mut m = HashMap::new();
            m.insert("テスト".to_string(), u64::MAX);
            m
        });
        predictor.save().unwrap();

        assert!(path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        // u64::MAX のスコアや context_score * 10 でもオーバーフローパニックしないこと
        let ranked = predictor.rank_candidates_borrowed(
            &["文脈"],
            "てすと",
            &["他候補", "テスト"],
            true,
        );
        assert_eq!(ranked[0], "テスト");
    }
}
