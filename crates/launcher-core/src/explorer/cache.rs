//! The explorer's cache of node answers: least recently used out first, weighed by bytes. An
//! entry over an eighth of the budget is not kept. Owned by the actor, so it needs no lock.

use std::{
    collections::{BTreeMap, HashMap},
    hash::Hash,
};

/// A byte-weighted least-recently-used map.
#[derive(Debug)]
pub(crate) struct Lru<K, V> {
    entries: HashMap<K, Entry<V>>,
    /// Use stamps, oldest first.
    order: BTreeMap<u64, K>,
    next_stamp: u64,
    bytes: usize,
    budget: usize,
}

#[derive(Debug)]
struct Entry<V> {
    value: V,
    stamp: u64,
    bytes: usize,
}

impl<K: Clone + Eq + Hash, V> Lru<K, V> {
    pub(crate) fn new(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: BTreeMap::new(),
            next_stamp: 0,
            bytes: 0,
            budget,
        }
    }

    /// The value for `key`, which becomes the most recently used.
    pub(crate) fn get(&mut self, key: &K) -> Option<&V> {
        let stamp = self.next_stamp;
        let entry = self.entries.get_mut(key)?;
        self.order.remove(&entry.stamp);
        entry.stamp = stamp;
        self.order.insert(stamp, key.clone());
        self.next_stamp += 1;
        Some(&entry.value)
    }

    /// Keeps `value`, weighing `bytes`, unless it is over an eighth of the budget. Evicts the
    /// least recently used entries until the cache fits.
    pub(crate) fn insert(&mut self, key: K, value: V, bytes: usize) {
        self.remove(&key);
        if bytes > self.budget / 8 {
            return;
        }
        let stamp = self.next_stamp;
        self.next_stamp += 1;
        self.order.insert(stamp, key.clone());
        self.entries.insert(
            key,
            Entry {
                value,
                stamp,
                bytes,
            },
        );
        self.bytes += bytes;
        self.shrink();
    }

    pub(crate) fn remove(&mut self, key: &K) -> Option<V> {
        let entry = self.entries.remove(key)?;
        self.order.remove(&entry.stamp);
        self.bytes -= entry.bytes;
        Some(entry.value)
    }

    /// Keeps only the entries `keep` accepts.
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&K, &V) -> bool) {
        let gone: Vec<K> = self
            .entries
            .iter()
            .filter(|(key, entry)| !keep(key, &entry.value))
            .map(|(key, _)| key.clone())
            .collect();
        for key in gone {
            self.remove(&key);
        }
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.bytes = 0;
    }

    /// A new budget, evicting at once if the cache is over it.
    pub(crate) fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
        self.shrink();
    }

    #[cfg(test)]
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    fn shrink(&mut self) {
        while self.bytes > self.budget {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            if let Some(entry) = self.entries.remove(&oldest) {
                self.bytes -= entry.bytes;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_least_recently_used_go_first() {
        let mut cache = Lru::new(800);
        cache.insert("a", 1, 100);
        cache.insert("b", 2, 100);
        cache.insert("c", 3, 100);
        assert_eq!(cache.bytes(), 300);
        // Reading `a` makes `b` the oldest.
        assert_eq!(cache.get(&"a"), Some(&1));
        cache.set_budget(200);
        assert_eq!(cache.get(&"b"), None);
        assert_eq!(cache.get(&"a"), Some(&1));
        assert_eq!(cache.get(&"c"), Some(&3));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn large_entries_are_not_kept() {
        let mut cache = Lru::new(800);
        cache.insert("big", 1, 101);
        assert_eq!(cache.get(&"big"), None);
        cache.insert("fits", 2, 100);
        assert_eq!(cache.get(&"fits"), Some(&2));
    }

    #[test]
    fn replacing_and_retaining() {
        let mut cache = Lru::new(8_000);
        cache.insert(1, "one", 10);
        cache.insert(1, "uno", 30);
        assert_eq!(cache.bytes(), 30);
        cache.insert(2, "two", 10);
        cache.retain(|key, _| *key != 1);
        assert_eq!(cache.get(&1), None);
        assert_eq!(cache.bytes(), 10);
        assert_eq!(cache.remove(&2), Some("two"));
        assert_eq!(cache.bytes(), 0);
        cache.insert(3, "three", 10);
        cache.clear();
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.bytes(), 0);
    }
}
