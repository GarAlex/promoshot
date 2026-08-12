//! MemoryGovernor: byte-budget accounting with LRU eviction. Portable and
//! resource-agnostic — the preview frame cache registers entries with their
//! byte cost and gets told which entries to drop when a new one would
//! overflow the budget. (RUST-CORE-PLAN §4: every cache lives under a
//! governor budget; nothing grows unbounded.)

use std::collections::HashMap;

/// An entry key: opaque to the governor.
pub type EntryId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    bytes: usize,
    last_used: u64,
}

/// LRU byte-budget bookkeeper.
pub struct MemoryGovernor {
    budget: usize,
    used: usize,
    tick: u64,
    entries: HashMap<EntryId, Entry>,
    evictions: u64,
}

impl MemoryGovernor {
    pub fn new(budget_bytes: usize) -> Self {
        MemoryGovernor {
            budget: budget_bytes,
            used: 0,
            tick: 0,
            entries: HashMap::new(),
            evictions: 0,
        }
    }

    pub fn budget(&self) -> usize {
        self.budget
    }

    pub fn used(&self) -> usize {
        self.used
    }

    pub fn evictions(&self) -> u64 {
        self.evictions
    }

    /// Registers `id` costing `bytes` and returns the entries to evict (LRU
    /// first) so the total fits the budget. The caller must drop those
    /// resources and MUST treat the returned ids as already unregistered.
    /// An entry larger than the whole budget is admitted alone — the video
    /// frame currently on screen must always be cacheable.
    #[must_use]
    pub fn admit(&mut self, id: EntryId, bytes: usize) -> Vec<EntryId> {
        self.tick += 1;
        if let Some(old) = self.entries.remove(&id) {
            self.used -= old.bytes;
        }
        let mut victims = Vec::new();
        while self.used + bytes > self.budget && !self.entries.is_empty() {
            let (&victim, _) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .expect("non-empty");
            let entry = self.entries.remove(&victim).expect("present");
            self.used -= entry.bytes;
            self.evictions += 1;
            victims.push(victim);
        }
        self.entries.insert(
            id,
            Entry {
                bytes,
                last_used: self.tick,
            },
        );
        self.used += bytes;
        victims
    }

    /// Marks `id` as freshly used (cache hit).
    pub fn touch(&mut self, id: EntryId) {
        self.tick += 1;
        let tick = self.tick;
        if let Some(e) = self.entries.get_mut(&id) {
            e.last_used = tick;
        }
    }

    /// Unregisters `id` (resource dropped by the caller for its own reasons).
    pub fn remove(&mut self, id: EntryId) {
        if let Some(e) = self.entries.remove(&id) {
            self.used -= e.bytes;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_lru_to_fit_budget() {
        let mut g = MemoryGovernor::new(100);
        assert!(g.admit(1, 40).is_empty());
        assert!(g.admit(2, 40).is_empty());
        g.touch(1); // 2 is now least-recently-used
        let victims = g.admit(3, 40);
        assert_eq!(victims, vec![2]);
        assert_eq!(g.used(), 80);
        assert_eq!(g.evictions(), 1);
    }

    #[test]
    fn oversized_entry_admitted_alone() {
        let mut g = MemoryGovernor::new(100);
        assert!(g.admit(1, 60).is_empty());
        let victims = g.admit(2, 500);
        assert_eq!(victims, vec![1]);
        assert_eq!(g.used(), 500);
    }

    #[test]
    fn re_admit_replaces_cost() {
        let mut g = MemoryGovernor::new(100);
        assert!(g.admit(1, 90).is_empty());
        assert!(g.admit(1, 30).is_empty());
        assert_eq!(g.used(), 30);
    }

    #[test]
    fn remove_frees_bytes() {
        let mut g = MemoryGovernor::new(100);
        let _ = g.admit(1, 70);
        g.remove(1);
        assert_eq!(g.used(), 0);
        assert!(g.admit(2, 100).is_empty());
    }
}
