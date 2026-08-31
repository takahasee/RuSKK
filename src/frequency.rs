use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::debug;

const SAVE_INTERVAL: u32 = 10;
const LEGACY_HISTORY_FILE: &str = ".skk-proxy-bayesian.json";

/// Frequency-based predictor for completion candidate ranking.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct FrequencyPredictor {
    /// Mapping of midashi -> (candidate -> count)
    pub frequencies: HashMap<String, HashMap<String, u64>>,
    /// Mapping of context_word (kanji/word) -> (candidate -> count)
    #[serde(default)]
    pub context_frequencies: HashMap<String, HashMap<String, u64>>,
    #[serde(skip)]
    pub storage_path: Option<PathBuf>,
    #[serde(skip)]
    pending_saves: u32,
}

impl FrequencyPredictor {
    pub fn new(storage_path: Option<PathBuf>) -> Self {
        let mut predictor = Self {
            frequencies: HashMap::new(),
            context_frequencies: HashMap::new(),
            storage_path,
            pending_saves: 0,
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
            let legacy = home.join(LEGACY_HISTORY_FILE);
            if legacy.exists() {
                debug!(from = %legacy.display(), to = %path.display(), "migrating legacy history file");
                return self.load(&legacy);
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

    /// Record selection of candidate given midashi and preceding kanji/word context.
    pub fn observe(&mut self, context: &[String], midashi: &str, candidate: &str) {
        let entry = self
            .frequencies
            .entry(midashi.to_string())
            .or_insert_with(HashMap::new);
        *entry.entry(candidate.to_string()).or_insert(0) += 1;

        for ctx in context {
            if !ctx.trim().is_empty() {
                let ctx_entry = self
                    .context_frequencies
                    .entry(ctx.to_string())
                    .or_insert_with(HashMap::new);
                *ctx_entry.entry(candidate.to_string()).or_insert(0) += 1;
            }
        }

        self.pending_saves += 1;
        if self.pending_saves >= SAVE_INTERVAL {
            let _ = self.save();
            self.pending_saves = 0;
        }
    }

    /// Rank candidates according to global frequency and context co-occurrence.
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

                // Give higher weight (x10) to context co-occurrence
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

        let ranked = predictor.rank_candidates(&[], "あい", &candidates);
        assert_eq!(ranked, vec!["愛", "相", "藍"]);

        predictor.observe(&[], "あい", "相");
        predictor.observe(&[], "あい", "相");
        predictor.observe(&[], "あい", "藍");
        predictor.observe(&[], "あい", "藍");
        predictor.observe(&[], "あい", "藍");
        predictor.observe(&[], "あい", "藍");
        predictor.observe(&[], "あい", "藍");

        let ranked_after = predictor.rank_candidates(&[], "あい", &candidates);
        assert_eq!(ranked_after, vec!["藍", "相", "愛"]);
    }

    #[test]
    fn kanji_context_cooccurrence_ranking() {
        let mut predictor = FrequencyPredictor::new(None);
        let candidates = vec!["着る".to_string(), "切る".to_string()];

        // Observe "服" -> "着る"
        predictor.observe(&["服".to_string()], "きる", "着る");
        predictor.observe(&["服".to_string()], "きる", "着る");

        // Observe "肉" -> "切る"
        predictor.observe(&["肉".to_string()], "きる", "切る");
        predictor.observe(&["肉".to_string()], "きる", "切る");
        predictor.observe(&["肉".to_string()], "きる", "切る");

        // When context is "服", "着る" should rank first
        let ranked_fuku = predictor.rank_candidates(&["服".to_string()], "きる", &candidates);
        assert_eq!(ranked_fuku[0], "着る");

        // When context is "肉", "切る" should rank first
        let ranked_niku = predictor.rank_candidates(&["肉".to_string()], "きる", &candidates);
        assert_eq!(ranked_niku[0], "切る");
    }
}

