use std::borrow::Borrow;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, RwLock};

/// 変換レスポンスのキャッシュエントリ
#[derive(Debug, Clone)]
pub struct CachedLookup {
    /// クライアントへ送信する完全なレスポンスバイト列（例: `1/切/着/伐/\n`）
    pub response: Arc<[u8]>,
    /// 直前確定文脈追跡用の第1候補（存在する場合）
    pub top_candidate: Option<Arc<str>>,
}

/// 変換キャッシュのキー
/// 文脈連動候補昇格が有効な場合、直前確定単語によって候補順が変わるため
/// `(context, midashi)` のペアをキーとする。
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// キャッシュ検索時に `String` をヒープ確保せず `(Option<&str>, &str)` で照合するためのトレイト
trait LookupKeyRef {
    fn key_pair(&self) -> (Option<&str>, &str);
}

impl LookupKeyRef for LookupKey {
    #[inline]
    fn key_pair(&self) -> (Option<&str>, &str) {
        (self.context.as_deref(), self.midashi.as_str())
    }
}

impl LookupKeyRef for (Option<&str>, &str) {
    #[inline]
    fn key_pair(&self) -> (Option<&str>, &str) {
        (self.0, self.1)
    }
}

impl PartialEq for dyn LookupKeyRef + '_ {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.key_pair() == other.key_pair()
    }
}

impl Eq for dyn LookupKeyRef + '_ {}

impl Hash for dyn LookupKeyRef + '_ {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key_pair().hash(state);
    }
}

impl Hash for LookupKey {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key_pair().hash(state);
    }
}

impl<'a> Borrow<dyn LookupKeyRef + 'a> for Arc<LookupKey> {
    #[inline]
    fn borrow(&self) -> &(dyn LookupKeyRef + 'a) {
        self.as_ref()
    }
}

/// スレッドセーフかつ高スループットなバウンデッド（容量制限付き）Lookup キャッシュ。
/// 読み取り時は read ロックのみを取得し、借用キー照合と `Arc` 共有でヒープ確保ゼロの応答を行う。
pub struct LookupCache {
    capacity: usize,
    inner: RwLock<CacheInner>,
}

struct CacheInner {
    map: HashMap<Arc<LookupKey>, CachedLookup>,
    order: VecDeque<Arc<LookupKey>>,
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

    /// 借用スライスのみでキャッシュエントリを取得する（完全ゼロアロケーション読み取りパス）
    pub fn get_by_ref(&self, context: Option<&str>, midashi: &str) -> Option<CachedLookup> {
        let guard = self.inner.read().ok()?;
        let key_ref: &(dyn LookupKeyRef + '_) = &(context, midashi);
        guard.map.get(key_ref).cloned()
    }

    /// キャッシュからエントリを取得する（高速読み取りパス）
    pub fn get(&self, key: &LookupKey) -> Option<CachedLookup> {
        self.get_by_ref(key.context.as_deref(), &key.midashi)
    }

    /// キャッシュにエントリを挿入する
    pub fn insert(&self, key: LookupKey, value: CachedLookup) {
        if self.capacity == 0 {
            return;
        }
        if let Ok(mut guard) = self.inner.write() {
            let key_ref: &(dyn LookupKeyRef + '_) =
                &(key.context.as_deref(), key.midashi.as_str());
            if let Some(existing) = guard.map.get_mut(key_ref) {
                *existing = value;
                return;
            }

            if guard.map.len() >= self.capacity
                && let Some(oldest) = guard.order.pop_front()
            {
                guard.map.remove(&oldest);
            }

            let arc_key = Arc::new(key);
            guard.order.push_back(Arc::clone(&arc_key));
            guard.map.insert(arc_key, value);
        }
    }

    /// 借用キーでキャッシュにエントリを挿入する（既存キー上書き時はヒープ確保ゼロ）
    pub fn insert_by_ref(&self, context: Option<&str>, midashi: &str, value: CachedLookup) {
        if self.capacity == 0 {
            return;
        }
        if let Ok(mut guard) = self.inner.write() {
            let key_ref: &(dyn LookupKeyRef + '_) = &(context, midashi);
            if let Some(existing) = guard.map.get_mut(key_ref) {
                *existing = value;
                return;
            }

            if guard.map.len() >= self.capacity
                && let Some(oldest) = guard.order.pop_front()
            {
                guard.map.remove(&oldest);
            }

            let arc_key = Arc::new(LookupKey::new(context, midashi));
            guard.order.push_back(Arc::clone(&arc_key));
            guard.map.insert(arc_key, value);
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
            response: Arc::from("1/切/着/\n".as_bytes()),
            top_candidate: Some(Arc::from("切")),
        };

        let k2 = LookupKey::new(Some("服"), "きr");
        let v2 = CachedLookup {
            response: Arc::from("1/着/切/\n".as_bytes()),
            top_candidate: Some(Arc::from("着")),
        };

        cache.insert(k1.clone(), v1.clone());
        cache.insert(k2.clone(), v2.clone());

        assert_eq!(cache.get(&k1).unwrap().top_candidate.as_deref(), Some("切"));
        assert_eq!(cache.get(&k2).unwrap().top_candidate.as_deref(), Some("着"));
        assert_eq!(
            cache.get_by_ref(Some("服"), "きr").unwrap().response.as_ref(),
            "1/着/切/\n".as_bytes()
        );

        // 既存キー更新でエントリ数が増えないこと
        let v2_updated = CachedLookup {
            response: Arc::from("1/着/切/伐/\n".as_bytes()),
            top_candidate: Some(Arc::from("着")),
        };
        cache.insert(k2.clone(), v2_updated);
        assert_eq!(cache.len(), 2);
        assert_eq!(
            cache.get_by_ref(Some("服"), "きr").unwrap().response.as_ref(),
            "1/着/切/伐/\n".as_bytes()
        );

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

        // capacity == 0 では挿入されないこと
        let zero_cache = LookupCache::new(0);
        zero_cache.insert(k1.clone(), v1.clone());
        assert_eq!(zero_cache.len(), 0);
        assert!(zero_cache.get(&k1).is_none());

        // insert_by_ref の新規挿入および既存上書きテスト
        let ref_cache = LookupCache::new(2);
        ref_cache.insert_by_ref(Some("肉"), "きr", v1);
        assert_eq!(ref_cache.len(), 1);
        assert_eq!(
            ref_cache.get_by_ref(Some("肉"), "きr").unwrap().top_candidate.as_deref(),
            Some("切")
        );
        ref_cache.insert_by_ref(
            Some("肉"),
            "きr",
            CachedLookup {
                response: Arc::from("1/切/伐/\n".as_bytes()),
                top_candidate: Some(Arc::from("切")),
            },
        );
        assert_eq!(ref_cache.len(), 1);
        assert_eq!(
            ref_cache.get_by_ref(Some("肉"), "きr").unwrap().response.as_ref(),
            "1/切/伐/\n".as_bytes()
        );
    }
}
