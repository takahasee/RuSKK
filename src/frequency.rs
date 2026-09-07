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
        if let Some(ref path) = predictor.storage_path.clone() {
            if let Err(err) = predictor.load_with_legacy_fallback(path) {
                debug!(error = %err, "no existing frequency data loaded, starting fresh");
            }
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

    /// macSKK などのユーザー辞書 (skk-jisyo.utf8) をパースし、
    /// 候補の順番に応じたスコアを frequencies にインポート（加算）する。
    /// context_frequencies は変更しない。
    pub fn import_from_skk_dict<P: AsRef<Path>>(&mut self, dict_path: P) -> anyhow::Result<()> {
        let content = fs::read_to_string(dict_path)?;
        
        for line in content.lines() {
            let line = line.trim();
            // コメント行や空行はスキップ
            if line.is_empty() || line.starts_with(';') {
                continue;
            }

            // フォーマット: 見出し語 /候補1/候補2/
            let mut parts = line.splitn(2, ' ');
            let midashi = match parts.next() {
                Some(m) if !m.is_empty() => m,
                _ => continue,
            };
            
            let cands_part = match parts.next() {
                Some(c) if c.starts_with('/') && c.ends_with('/') => &c[1..c.len() - 1],
                _ => continue,
            };

            let candidates: Vec<&str> = cands_part.split('/').collect();
            if candidates.is_empty() {
                continue;
            }

            let entry = self.frequencies.entry(midashi.to_string()).or_insert_with(HashMap::new);
            
            // 順番に応じてスコアを付与 (先頭ほど高い)
            // 候補がN個の場合、1番目=N点, 2番目=N-1点, ..., N番目=1点
            let total_cands = candidates.len() as u64;
            for (idx, cand) in candidates.iter().enumerate() {
                // 注釈付き候補 (例: 候補;注釈) の場合は候補部分だけを取り出す
                let cand_text = cand.split(';').next().unwrap_or(cand);
                if cand_text.is_empty() {
                    continue;
                }
                
                let score = total_cands.saturating_sub(idx as u64);
                *entry.entry(cand_text.to_string()).or_insert(0) += score;
            }
        }
        
        // インポートした結果を保存する
        self.save()?;
        
        Ok(())
    }

    /// seed データの頻度と文脈共起に基づいて候補を並び替える。
    pub fn rank_candidates(&self, context: &[String], midashi: &str, candidates: &[String]) -> Vec<String> {
        if candidates.len() <= 1 {
            return candidates.to_vec();
        }

        let empty_map = HashMap::new();
        let freq_map = self.frequencies.get(midashi).unwrap_or(&empty_map);

        let mut indexed_cands: Vec<(usize, &String, u64)> = candidates
            .iter()
            .enumerate()
            .map(|(idx, cand)| {
                let global_count = freq_map.get(cand).copied().unwrap_or(0);
                let context_score: u64 = context
                    .iter()
                    .map(|ctx| {
                        self.context_frequencies
                            .get(ctx)
                            .and_then(|m| m.get(cand))
                            .copied()
                            .unwrap_or(0)
                    })
                    .sum();

                // 文脈共起は10倍の重みで評価する
                let total_score = global_count + context_score * 10;
                (idx, cand, total_score)
            })
            .collect();

        indexed_cands.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));

        indexed_cands
            .into_iter()
            .map(|(_, cand, _)| cand.clone())
            .collect()
    }
}

#[derive(Serialize, Deserialize)]
struct FrequencyPredictorData {
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
        let candidates = vec!["着る".to_string(), "切る".to_string()];

        // seed データを直接設定（手動編集を模倣）
        predictor.context_frequencies.insert("服".to_string(), {
            let mut m = HashMap::new();
            m.insert("着る".to_string(), 2);
            m
        });
        predictor.context_frequencies.insert("肉".to_string(), {
            let mut m = HashMap::new();
            m.insert("切る".to_string(), 3);
            m
        });

        // 文脈「服」のとき、「着る」が第1候補
        let ranked_fuku = predictor.rank_candidates(&["服".to_string()], "きる", &candidates);
        assert_eq!(ranked_fuku[0], "着る");

        // 文脈「肉」のとき、「切る」が第1候補
        let ranked_niku = predictor.rank_candidates(&["肉".to_string()], "きる", &candidates);
        assert_eq!(ranked_niku[0], "切る");
    }

    #[test]
    fn test_import_from_skk_dict() {
        use tempfile::NamedTempFile;
        use std::io::Write;

        // ダミーの SKK 辞書ファイルを作成
        let mut dict_file = NamedTempFile::new().unwrap();
        writeln!(dict_file, ";; okuri-ari entries.").unwrap();
        writeln!(dict_file, "きr /切る;注釈/着る/伐る/").unwrap();
        writeln!(dict_file, ";; okuri-nasi entries.").unwrap();
        writeln!(dict_file, "ふく /服/吹く/副/").unwrap();
        
        // テスト用の JSON 保存先
        let json_file = NamedTempFile::new().unwrap();

        let mut predictor = FrequencyPredictor::new(Some(json_file.path().to_path_buf()));
        // 既存の context_frequencies が保持されることを確認するため追加
        predictor.context_frequencies.insert("肉".to_string(), {
            let mut m = HashMap::new();
            m.insert("切る".to_string(), 10);
            m
        });

        // インポート実行
        predictor.import_from_skk_dict(dict_file.path()).unwrap();

        // frequencies が正しくインポートされているか確認
        // "きr": 切る=3, 着る=2, 伐る=1
        let kir_freqs = predictor.frequencies.get("きr").unwrap();
        assert_eq!(kir_freqs.get("切る").copied().unwrap_or(0), 3);
        assert_eq!(kir_freqs.get("着る").copied().unwrap_or(0), 2);
        assert_eq!(kir_freqs.get("伐る").copied().unwrap_or(0), 1);

        // "ふく": 服=3, 吹く=2, 副=1
        let fuku_freqs = predictor.frequencies.get("ふく").unwrap();
        assert_eq!(fuku_freqs.get("服").copied().unwrap_or(0), 3);
        assert_eq!(fuku_freqs.get("吹く").copied().unwrap_or(0), 2);
        assert_eq!(fuku_freqs.get("副").copied().unwrap_or(0), 1);

        // context_frequencies が維持されているか確認
        let niku_ctx = predictor.context_frequencies.get("肉").unwrap();
        assert_eq!(niku_ctx.get("切る").copied().unwrap_or(0), 10);

        // ファイルにも保存されているか確認
        let mut loaded = FrequencyPredictor::new(Some(json_file.path().to_path_buf()));
        loaded.load(json_file.path()).unwrap();
        assert_eq!(loaded.frequencies.get("きr").unwrap().get("切る").copied().unwrap_or(0), 3);
    }
}
