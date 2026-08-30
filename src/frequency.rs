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
    #[serde(skip)]
    pub storage_path: Option<PathBuf>,
    #[serde(skip)]
    pending_saves: u32,
}

impl FrequencyPredictor {
    pub fn new(storage_path: Option<PathBuf>) -> Self {
        let mut predictor = Self {
            frequencies: HashMap::new(),
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
        let loaded: HashMap<String, HashMap<String, u64>> = serde_json::from_str(&data)?;
        self.frequencies = loaded;
        Ok(())
    }

    pub fn save(&self) -> anyhow::Result<()> {
        if let Some(ref path) = self.storage_path {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let data = serde_json::to_string_pretty(&self.frequencies)?;
            fs::write(path, data)?;
        }
        Ok(())
    }

    /// Record an implicit selection when lookup returns exactly one candidate.
    pub fn observe(&mut self, midashi: &str, candidate: &str) {
        let entry = self
            .frequencies
            .entry(midashi.to_string())
            .or_insert_with(HashMap::new);
        *entry.entry(candidate.to_string()).or_insert(0) += 1;
        self.pending_saves += 1;
        if self.pending_saves >= SAVE_INTERVAL {
            let _ = self.save();
            self.pending_saves = 0;
        }
    }

    /// Rank candidates according to historical selection frequencies.
    pub fn rank_candidates(&self, midashi: &str, candidates: &[String]) -> Vec<String> {
        if candidates.len() <= 1 {
            return candidates.to_vec();
        }

        let empty_map = HashMap::new();
        let freq_map = self.frequencies.get(midashi).unwrap_or(&empty_map);

        let mut indexed_cands: Vec<(usize, &String, u64)> = candidates
            .iter()
            .enumerate()
            .map(|(idx, cand)| {
                let count = freq_map.get(cand).copied().unwrap_or(0);
                (idx, cand, count)
            })
            .collect();

        indexed_cands.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));

        indexed_cands
            .into_iter()
            .map(|(_, cand, _)| cand.clone())
            .collect()
    }
}

pub type SharedPredictor = Arc<Mutex<FrequencyPredictor>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predictor_ranking_by_frequency() {
        let mut predictor = FrequencyPredictor::new(None);
        let candidates = vec!["愛".to_string(), "相".to_string(), "藍".to_string()];

        let ranked = predictor.rank_candidates("あい", &candidates);
        assert_eq!(ranked, vec!["愛", "相", "藍"]);

        predictor.observe("あい", "相");
        predictor.observe("あい", "相");
        predictor.observe("あい", "藍");
        predictor.observe("あい", "藍");
        predictor.observe("あい", "藍");
        predictor.observe("あい", "藍");
        predictor.observe("あい", "藍");

        let ranked_after = predictor.rank_candidates("あい", &candidates);
        assert_eq!(ranked_after, vec!["藍", "相", "愛"]);
    }
}
