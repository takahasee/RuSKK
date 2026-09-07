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
}
