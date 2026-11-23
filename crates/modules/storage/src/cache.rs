//! Disposable caches. Runtime documents remain authoritative.
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// Largest decoded record kept by [`CountedRecords`], in serialized bytes.
const COUNTED_RECORD_MAX: usize = 1024 * 1024;
/// Bytes of decoded records one account keeps.
const COUNTED_RECORDS_BUDGET: usize = 4 * 1024 * 1024;

/// Decoded documents of one account database that have a write counter (session records,
/// session modes and ability states, run records with their split input), each tagged with
/// the counter it was read at (stream B6, `webagent_storage::revisions::counted`). An entry is used only when
/// the reader's own fresh read of that counter, in the reader's own statement or transaction,
/// equals the tag: every write of the record moves the counter inside the writing transaction
/// (`revisions.rs` triggers), so an equal counter means the same committed record. Entries
/// are added only from committed reads; nothing is reused by time. Lives inside the account's
/// [`ContextCache`], so it goes with the account's pool (a replaced database file is opened
/// as a new plane with an empty cache).
#[derive(Default)]
pub struct CountedRecords {
    entries: HashMap<String, CountedRecord>,
    weight: usize,
    clock: u64,
}

struct CountedRecord {
    counter: i64,
    value: Arc<Value>,
    weight: usize,
    stamp: u64,
}

impl CountedRecords {
    /// The document `key` when it was cached at exactly `counter`.
    pub fn get(&mut self, key: &str, counter: i64) -> Option<Arc<Value>> {
        let entry = self.entries.get_mut(key)?;
        if entry.counter != counter {
            return None;
        }
        self.clock = self.clock.wrapping_add(1);
        entry.stamp = self.clock;
        Some(entry.value.clone())
    }

    /// Keep `value`, read in one committed snapshot together with `counter`. An entry read at
    /// a higher counter is kept instead (both are exact; the newer one is more useful).
    pub fn insert(&mut self, key: &str, counter: i64, value: Arc<Value>) {
        if self
            .entries
            .get(key)
            .is_some_and(|entry| entry.counter > counter)
        {
            return;
        }
        self.remove(key);
        let weight = serde_json::to_vec(&*value)
            .map(|bytes| bytes.len().saturating_add(128))
            .unwrap_or(usize::MAX);
        if weight > COUNTED_RECORD_MAX {
            return;
        }
        while self.weight.saturating_add(weight) > COUNTED_RECORDS_BUDGET {
            let Some(victim) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.stamp)
                .map(|(key, _)| key.clone())
            else {
                return;
            };
            self.remove(&victim);
        }
        self.clock = self.clock.wrapping_add(1);
        self.weight += weight;
        self.entries.insert(
            key.to_owned(),
            CountedRecord {
                counter,
                value,
                weight,
                stamp: self.clock,
            },
        );
    }

    pub fn remove(&mut self, key: &str) {
        if let Some(entry) = self.entries.remove(key) {
            self.weight -= entry.weight;
        }
    }
}
/// Disposable, bounded assembled context. SQLite checkpoints and interactions
/// remain authoritative; this only avoids decrypting old interaction rows.
pub struct ContextCache {
    entries: HashMap<String, ContextEntry>,
    budget: usize,
    weight: usize,
    clock: u64,
    /// The account's counter-checked decoded documents (stream B6).
    pub counted_records: CountedRecords,
}

struct ContextEntry {
    checkpoint_through: i64,
    history_revision: u64,
    cursor: i64,
    input: Vec<Value>,
    weight: usize,
    stamp: u64,
}

impl ContextCache {
    pub fn new(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            budget,
            weight: 0,
            clock: 0,
            counted_records: CountedRecords::default(),
        }
    }

    pub fn get(
        &mut self,
        sid: &str,
        checkpoint_through: i64,
        history_revision: u64,
    ) -> Option<(Vec<Value>, i64)> {
        if self.entries.get(sid)?.checkpoint_through != checkpoint_through
            || self.entries.get(sid)?.history_revision != history_revision
        {
            self.invalidate(sid);
            return None;
        }
        self.clock = self.clock.wrapping_add(1);
        let entry = self.entries.get_mut(sid)?;
        entry.stamp = self.clock;
        Some((entry.input.clone(), entry.cursor))
    }

    pub fn insert(
        &mut self,
        sid: String,
        checkpoint_through: i64,
        history_revision: u64,
        cursor: i64,
        input: Vec<Value>,
    ) {
        if self.entries.get(&sid).is_some_and(|entry| {
            entry.history_revision > history_revision
                || (entry.history_revision == history_revision
                    && entry.checkpoint_through == checkpoint_through
                    && entry.cursor > cursor)
        }) {
            return;
        }
        let weight = serde_json::to_vec(&input)
            .map(|bytes| bytes.len().saturating_mul(4).saturating_add(256))
            .unwrap_or(usize::MAX);
        self.invalidate(&sid);
        if weight > self.budget {
            return;
        }
        while self.weight.saturating_add(weight) > self.budget {
            let victim = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.stamp)
                .map(|(sid, _)| sid.clone());
            if let Some(victim) = victim {
                self.invalidate(&victim);
            } else {
                return;
            }
        }
        self.clock = self.clock.wrapping_add(1);
        self.weight += weight;
        self.entries.insert(
            sid,
            ContextEntry {
                checkpoint_through,
                history_revision,
                cursor,
                input,
                weight,
                stamp: self.clock,
            },
        );
    }

    pub fn invalidate(&mut self, sid: &str) {
        if let Some(entry) = self.entries.remove(sid) {
            self.weight -= entry.weight;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn context_cache_appends_without_regressing_cursor_and_evicts() {
        let mut cache = ContextCache::new(1200);
        cache.insert("a".into(), 0, 0, 1, vec![json!("a".repeat(20))]);
        cache.insert("a".into(), 0, 0, 2, vec![json!("a".repeat(20)), json!("b")]);
        cache.insert("a".into(), 0, 0, 1, vec![json!("stale")]);
        assert_eq!(cache.get("a", 0, 0).unwrap().1, 2);
        cache.insert("b".into(), 0, 0, 1, vec![json!("b".repeat(200))]);
        assert!(cache.weight <= cache.budget);
        assert!(cache.get("a", 1, 0).is_none());
    }

    #[test]
    fn oversized_context_is_not_cached() {
        let mut cache = ContextCache::new(128);
        cache.insert("a".into(), 0, 0, 1, vec![json!("x".repeat(100))]);
        assert!(cache.get("a", 0, 0).is_none());
    }
}
