use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::debug;

const LEGACY_HISTORY_FILES: &[&str] = &[
    ".skk-proxy-frequency.json",
    ".skk-proxy-bayesian.json",
];

/// 読み取り専用の頻度・文脈データに基づいて候補を並び替える。
/// skkserv プロトコルではユーザーの選択を知る手段がないため、自動学習は行わない。
/// ユーザーが `~/.ruskk-frequency.json` を手動で編集し、seed データを投入する。
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
            storage_path,
        };
        if let Some(ref path) = predictor.storage_path.clone()
            && let Err(err) = predictor.load_with_legacy_fallback(path) {
                debug!(error = %err, "no existing frequency data loaded, starting fresh");
            }
        predictor
    }

    fn load_with_legacy_fallback(&mut self, path: &Path) -> anyhow::Result<()> {
        if path.exists() {
            return self.load(path);
        }
        if let Some(home) = path.parent() {
            for legacy_name in LEGACY_HISTORY_FILES {
                let legacy = home.join(legacy_name);
                if legacy.exists() {
                    debug!(from = %legacy.display(), to = %path.display(), "migrating legacy history file");
                    return self.load(&legacy);
                }
            }
        }
        Ok(())
    }

    pub fn load<P: AsRef<Path>>(&mut self, path: P) -> anyhow::Result<()> {
        if !path.as_ref().exists() {
            return Ok(());
        }
        let data = fs::read_to_string(path)?;
        let loaded: FrequencyPredictorData = serde_json::from_str(&data)?;
        self.frequencies = loaded.frequencies;
        self.context_frequencies = loaded.context_frequencies.unwrap_or_default();
        Ok(())
    }

    pub fn save(&self) -> anyhow::Result<()> {
        if let Some(ref path) = self.storage_path {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let data = serde_json::to_string_pretty(&self)?;
            fs::write(path, data)?;
        }
        Ok(())
    }

    pub const DEFAULT_SEED_JSON: &'static str = include_str!("../data/default-seed.json");

    /// 組み込みのデフォルト文脈プリセットをパースして返す。
    pub fn default_preset_data() -> anyhow::Result<FrequencyPredictorData> {
        let data: FrequencyPredictorData = serde_json::from_str(Self::DEFAULT_SEED_JSON)?;
        Ok(data)
    }

    /// 組み込みのデフォルト文脈プリセット（context_frequencies）をマージする。
    /// 既存の frequencies や context_frequencies は維持され、プリセット側の値が追加・更新（最大値）される。
    pub fn merge_default_presets(&mut self) -> anyhow::Result<()> {
        let preset = Self::default_preset_data()?;
        if let Some(ctx_map) = preset.context_frequencies {
            for (ctx, cands) in ctx_map {
                let entry = self.context_frequencies.entry(ctx).or_default();
                for (cand, count) in cands {
                    let current = entry.entry(cand).or_insert(0);
                    *current = (*current).max(count);
                }
            }
        }
        Ok(())
    }

    /// seed ファイル（~/.ruskk-frequency.json）を初期化またはマージして保存する。
    /// force が true の場合はプリセットで完全上書きし、false の場合は既存データを保持したままマージする。
    pub fn init_seed(&mut self, force: bool) -> anyhow::Result<()> {
        if force {
            let preset = Self::default_preset_data()?;
            self.frequencies = preset.frequencies;
            self.context_frequencies = preset.context_frequencies.unwrap_or_default();
        } else {
            self.merge_default_presets()?;
        }
        self.save()?;
        Ok(())
    }

    /// macSKK などのユーザー辞書 (skk-jisyo.utf8) をパースし、
    /// 候補の順番に応じたスコアを frequencies にインポート（加算）する。
    /// 送りありブロック（例: `きr /[る/着/切/伐/]/[り/切/]/`）も正確にパースし、
    /// `"[る"` や `"]"` などの不要記号の混入を完全に防止する。
    /// context_frequencies が空の場合はデフォルトプリセットも自動注入する。
    pub fn import_from_skk_dict<P: AsRef<Path>>(&mut self, dict_path: P) -> anyhow::Result<()> {
        let content = fs::read_to_string(dict_path)?;
        
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
            let total_cands = candidates.len() as u64;
            for (idx, cand_text) in candidates.iter().enumerate() {
                let score = total_cands.saturating_sub(idx as u64);
                *entry.entry(cand_text.clone()).or_insert(0) += score;
            }
        }
        
        // context_frequencies が空なら、デフォルトプリセットも一緒に初期化
        if self.context_frequencies.is_empty() {
            let _ = self.merge_default_presets();
        }

        // インポートした結果を保存する
        self.save()?;
        
        Ok(())
    }

    /// seed データの頻度と文脈共起に基づいて候補を並び替える。
    /// 候補文字列に注釈（`;` 以降）が含まれる場合や、
    /// 送りあり単漢字（"切"）と活用形（"切る"）の表記差がある場合も柔軟にスコアを照合する。
    pub fn rank_candidates(&self, context: &[String], midashi: &str, candidates: &[String]) -> Vec<String> {
        if candidates.len() <= 1 {
            return candidates.to_vec();
        }

        let empty_map = HashMap::new();
        let freq_map = self.frequencies.get(midashi).unwrap_or(&empty_map);
        let has_context = !context.is_empty() && !self.context_frequencies.is_empty();

        // 頻度情報も文脈情報もない場合は、無駄な計算やソートを完全スキップして即座に返却
        if freq_map.is_empty() && !has_context {
            return candidates.to_vec();
        }

        let mut indexed_cands: Vec<(usize, &String, u64)> = candidates
            .iter()
            .enumerate()
            .map(|(idx, cand)| {
                let clean = clean_candidate(cand);
                let global_count = get_score_flexible(freq_map, clean);
                let context_score: u64 = if has_context {
                    context
                        .iter()
                        .map(|ctx| {
                            self.context_frequencies
                                .get(ctx)
                                .map(|m| get_score_flexible(m, clean))
                                .unwrap_or(0)
                        })
                        .sum()
                } else {
                    0
                };

                // 文脈共起は10倍の重みで評価する
                let total_score = global_count + context_score * 10;
                (idx, cand, total_score)
            })
            .collect();

        // 全ての候補のスコアが 0 の場合は元の順序を維持
        if indexed_cands.iter().all(|(_, _, score)| *score == 0) {
            return candidates.to_vec();
        }

        indexed_cands.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));

        indexed_cands
            .into_iter()
            .map(|(_, cand, _)| cand.clone())
            .collect()
    }
}

/// マップからスコアを取得する。完全一致を最優先し、
/// 見つからない場合は送りあり単漢字（"切"）と活用形（"切る"）の前方一致を許容する。
fn get_score_flexible(map: &HashMap<String, u64>, target: &str) -> u64 {
    if let Some(&score) = map.get(target) {
        return score;
    }
    map.iter()
        .filter(|(k, _)| k.starts_with(target) || target.starts_with(k.as_str()))
        .map(|(_, &score)| score)
        .max()
        .unwrap_or(0)
}

/// 候補文字列から注釈（`;` 以降）を除去した本体文字列を返す。
pub fn clean_candidate(cand: &str) -> &str {
    cand.split(';').next().unwrap_or(cand).trim()
}

/// SKK 辞書エントリの候補部分（`/[る/着/切/伐/]/[り/切/]/` や `/変換/返還/`）から、
/// 送り仮名ブロック `[...]` や注釈 `;...` を解析し、純粋な候補文字列のリストを返す。
pub fn parse_dict_line_candidates(cands_part: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let trimmed = cands_part.trim_matches('/');
    if trimmed.is_empty() {
        return candidates;
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
            if !cand_text.is_empty() && !candidates.iter().any(|c| c == cand_text) {
                candidates.push(cand_text.to_string());
            }
        }

        if ends_bracket {
            in_bracket = false;
        }
    }

    candidates
}

#[derive(Serialize, Deserialize)]
pub struct FrequencyPredictorData {
    pub frequencies: HashMap<String, HashMap<String, u64>>,
    #[serde(default)]
    pub context_frequencies: Option<HashMap<String, HashMap<String, u64>>>,
}


pub type SharedPredictor = Arc<Mutex<FrequencyPredictor>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predictor_ranking_by_frequency() {
        let mut predictor = FrequencyPredictor::new(None);
        let candidates = vec!["愛".to_string(), "相".to_string(), "藍".to_string()];

        // 初期状態ではスコアがないため元の順序を維持
        let ranked = predictor.rank_candidates(&[], "あい", &candidates);
        assert_eq!(ranked, vec!["愛", "相", "藍"]);

        // seed データを直接設定（手動編集を模倣）
        predictor.frequencies.insert("あい".to_string(), {
            let mut m = HashMap::new();
            m.insert("相".to_string(), 2);
            m.insert("藍".to_string(), 7);
            m
        });

        let ranked_after = predictor.rank_candidates(&[], "あい", &candidates);
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

        // 1. 文脈「肉」のとき、「切る」（および語幹「切」）が第1候補
        let ranked_niku = predictor.rank_candidates(&["肉".to_string()], "きる", &candidates);
        assert_eq!(ranked_niku[0], "切る");
        let ranked_niku_stem = predictor.rank_candidates(&["肉".to_string()], "きr", &stem_candidates);
        assert_eq!(ranked_niku_stem[0], "切");

        // 2. 文脈「服」のとき、「着る」（および語幹「着」）が第1候補
        let ranked_fuku = predictor.rank_candidates(&["服".to_string()], "きる", &candidates);
        assert_eq!(ranked_fuku[0], "着る");
        let ranked_fuku_stem = predictor.rank_candidates(&["服".to_string()], "きr", &stem_candidates);
        assert_eq!(ranked_fuku_stem[0], "着");

        // 3. 文脈「木」のとき、「伐る」（および語幹「伐」）が第1候補
        let ranked_ki = predictor.rank_candidates(&["木".to_string()], "きる", &candidates);
        assert_eq!(ranked_ki[0], "伐る");
        let ranked_ki_stem = predictor.rank_candidates(&["木".to_string()], "きr", &stem_candidates);
        assert_eq!(ranked_ki_stem[0], "伐");
    }

    #[test]
    fn test_import_from_skk_dict() {
        use tempfile::NamedTempFile;
        use std::io::Write;

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
        let ranked = predictor.rank_candidates(&[], "きr", &["伐".to_string(), "切;注釈あり".to_string(), "着".to_string()]);
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
        assert_eq!(loaded.frequencies.get("きr").unwrap().get("着").copied().unwrap_or(0), 3);
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
            predictor.context_frequencies.get("服").unwrap().get("着る").copied().unwrap_or(0),
            10
        );
        assert_eq!(
            predictor.context_frequencies.get("肉").unwrap().get("切る").copied().unwrap_or(0),
            10
        );
        assert_eq!(
            predictor.context_frequencies.get("時間").unwrap().get("計る").copied().unwrap_or(0),
            10
        );

        // ファイルから再ロードしても正しく永続化されていること
        let reloaded = FrequencyPredictor::new(Some(json_file.path().to_path_buf()));
        assert_eq!(
            reloaded.context_frequencies.get("服").unwrap().get("着る").copied().unwrap_or(0),
            10
        );
    }
}
