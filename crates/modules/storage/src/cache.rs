//! Disposable caches. Runtime documents remain authoritative.
use serde_json::Value;
use std::collections::HashMap;
/// The retired document cache. Nothing ever read from or wrote to it, so it holds
/// nothing; `invalidate` stays a no-op only so the existing call sites compile until
/// they are removed with the `document_budget_bytes` setting.
pub struct Cache;
impl Cache {
    pub fn new(_budget: usize) -> Self {
        Self
    }
    pub fn invalidate(&mut self, _keys: &[String]) {}
}

/// Disposable, bounded assembled context. SQLite checkpoints and interactions
/// remain authoritative; this only avoids decrypting old interaction rows.
pub struct ContextCache {
    entries: HashMap<String, ContextEntry>,
    budget: usize,
    weight: usize,
    clock: u64,
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
