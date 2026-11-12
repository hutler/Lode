//! A small bounded least-recently-used map.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::sync::Mutex;

struct Inner<K, V> {
    map: HashMap<K, (V, u64)>,
    order: BTreeMap<u64, K>,
    tick: u64,
}

/// A thread-safe LRU cache holding at most `capacity` entries.
pub struct Lru<K, V> {
    capacity: usize,
    inner: Mutex<Inner<K, V>>,
}

impl<K: Hash + Eq + Clone, V: Clone> Lru<K, V> {
    /// An empty cache. A capacity of 0 is treated as 1.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            inner: Mutex::new(Inner {
                map: HashMap::new(),
                order: BTreeMap::new(),
                tick: 0,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner<K, V>> {
        // Entries are plain values; a panic elsewhere cannot leave them inconsistent.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The value for `k`, marking it most recently used.
    pub fn get(&self, k: &K) -> Option<V> {
        let mut g = self.lock();
        let inner = &mut *g;
        inner.tick += 1;
        let tick = inner.tick;
        let (v, t) = inner.map.get_mut(k)?;
        inner.order.remove(t);
        *t = tick;
        inner.order.insert(tick, k.clone());
        Some(v.clone())
    }

    /// Inserts or replaces, evicting the least recently used entry when full.
    pub fn insert(&self, k: K, v: V) {
        let mut g = self.lock();
        let inner = &mut *g;
        inner.tick += 1;
        let tick = inner.tick;
        if let Some((_, old)) = inner.map.insert(k.clone(), (v, tick)) {
            inner.order.remove(&old);
        }
        inner.order.insert(tick, k);
        while inner.map.len() > self.capacity {
            let Some((_, oldest)) = inner.order.pop_first() else {
                break;
            };
            inner.map.remove(&oldest);
        }
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.lock().map.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_least_recently_used() {
        let l = Lru::new(2);
        l.insert(1, "a");
        l.insert(2, "b");
        assert_eq!(l.get(&1), Some("a"));
        l.insert(3, "c");
        assert_eq!(l.get(&2), None);
        assert_eq!(l.get(&1), Some("a"));
        assert_eq!(l.get(&3), Some("c"));
        l.insert(3, "d");
        assert_eq!(l.len(), 2);
        assert_eq!(l.get(&3), Some("d"));
    }
}
