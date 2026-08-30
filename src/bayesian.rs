use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tracing::debug;

/// Bayesian frequency-based predictor for candidate ranking.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct BayesianPredictor {
    /// Mapping of midashi -> (candidate -> count)
    pub frequencies: HashMap<String, HashMap<String, u64>>,
    #[serde(skip)]
    pub storage_path: Option<PathBuf>,
}

impl BayesianPredictor {
    pub fn new(storage_path: Option<PathBuf>) -> Self {
        let mut predictor = Self {
            frequencies: HashMap::new(),
            storage_path,
        };
        if let Some(ref path) = predictor.storage_path.clone() {
            if let Err(err) = predictor.load(path) {
                debug!(error = %err, "no existing bayesian data loaded, starting fresh");
            }
        }
        predictor
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

    /// Record selection of candidate for midashi
    pub fn observe(&mut self, midashi: &str, candidate: &str) {
        let entry = self
            .frequencies
            .entry(midashi.to_string())
            .or_insert_with(HashMap::new);
        *entry.entry(candidate.to_string()).or_insert(0) += 1;
        let _ = self.save();
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

        // Sort descending by count, then ascending by original index (stable sort)
        indexed_cands.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));

        indexed_cands.into_iter().map(|(_, cand, _)| cand.clone()).collect()
    }
}

pub type SharedPredictor = Arc<Mutex<BayesianPredictor>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_predictor_ranking() {
        let mut predictor = BayesianPredictor::new(None);
        let candidates = vec!["愛".to_string(), "相".to_string(), "藍".to_string()];

        // Initial ranking should preserve original order
        let ranked = predictor.rank_candidates("あい", &candidates);
        assert_eq!(ranked, vec!["愛", "相", "藍"]);

        // Observe "相" twice and "藍" 5 times
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
