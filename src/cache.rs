use std::collections::{HashMap, VecDeque};
use std::sync::RwLock;

/// 変換レスポンスのキャッシュエントリ
#[derive(Debug, Clone)]
pub struct CachedLookup {
    /// クライアントへ送信する完全なレスポンスバイト列（例: `1/切/着/伐/\n`）
    pub response: Vec<u8>,
    /// 直前確定文脈追跡用の第1候補（存在する場合）
    pub top_candidate: Option<String>,
}

/// 変換キャッシュのキー
/// 文脈連動候補昇格が有効な場合、直前確定単語によって候補順が変わるため
/// `(context, midashi)` のペアをキーとする。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LookupKey {
    pub context: Option<String>,
    pub midashi: String,
}

impl LookupKey {
    #[inline]
    pub fn new(context: Option<&str>, midashi: &str) -> Self {
        Self {
            context: context.map(|s| s.to_string()),
            midashi: midashi.to_string(),
        }
    }
}

/// スレッドセーフかつ高スループットなバウンデッド（容量制限付き）Lookup キャッシュ。
/// 読み取り時は read ロックのみを取得し、ナノ秒オーダーでキャッシュヒットを返却する。
pub struct LookupCache {
    capacity: usize,
    inner: RwLock<CacheInner>,
}

struct CacheInner {
    map: HashMap<LookupKey, CachedLookup>,
    order: VecDeque<LookupKey>,
}

impl LookupCache {
    pub const DEFAULT_CAPACITY: usize = 4_096;

    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            inner: RwLock::new(CacheInner {
                map: HashMap::with_capacity(capacity.min(1024)),
                order: VecDeque::with_capacity(capacity.min(1024)),
            }),
        }
    }

    /// キャッシュからエントリを取得する（高速読み取りパス）
    pub fn get(&self, key: &LookupKey) -> Option<CachedLookup> {
        let guard = self.inner.read().ok()?;
        guard.map.get(key).cloned()
    }

    /// キャッシュにエントリを挿入する
    pub fn insert(&self, key: LookupKey, value: CachedLookup) {
        if let Ok(mut guard) = self.inner.write() {
            if let std::collections::hash_map::Entry::Occupied(mut e) = guard.map.entry(key.clone()) {
                e.insert(value);
                return;
            }

            if guard.map.len() >= self.capacity
                && let Some(oldest) = guard.order.pop_front()
            {
                guard.map.remove(&oldest);
            }

            guard.order.push_back(key.clone());
            guard.map.insert(key, value);
        }
    }

    /// SIGHUP リロード時やユーザー辞書更新時にキャッシュを全消去する
    pub fn clear(&self) {
        if let Ok(mut guard) = self.inner.write() {
            guard.map.clear();
            guard.order.clear();
        }
    }

    /// 現在のキャッシュエントリ数を取得する
    pub fn len(&self) -> usize {
        self.inner.read().map(|g| g.map.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for LookupCache {
    fn default() -> Self {
        Self::new(Self::DEFAULT_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lookup_cache_basic_and_context_separation() {
        let cache = LookupCache::new(3);

        let k1 = LookupKey::new(None, "きr");
        let v1 = CachedLookup {
            response: "1/切/着/\n".as_bytes().to_vec(),
            top_candidate: Some("切".into()),
        };

        let k2 = LookupKey::new(Some("服"), "きr");
        let v2 = CachedLookup {
            response: "1/着/切/\n".as_bytes().to_vec(),
            top_candidate: Some("着".into()),
        };

        cache.insert(k1.clone(), v1.clone());
        cache.insert(k2.clone(), v2.clone());

        assert_eq!(cache.get(&k1).unwrap().top_candidate.as_deref(), Some("切"));
        assert_eq!(cache.get(&k2).unwrap().top_candidate.as_deref(), Some("着"));

        // 容量超過テスト（3件目、4件目追加で最古の k1 が追い出される）
        let k3 = LookupKey::new(None, "にほん");
        cache.insert(k3.clone(), v1.clone());
        assert_eq!(cache.len(), 3);

        let k4 = LookupKey::new(None, "とうきょう");
        cache.insert(k4.clone(), v1.clone());
        assert_eq!(cache.len(), 3);
        assert!(cache.get(&k1).is_none()); // k1 は退去済み
        assert!(cache.get(&k2).is_some());

        // clear テスト
        cache.clear();
        assert_eq!(cache.len(), 0);
        assert!(cache.get(&k2).is_none());
    }
}
