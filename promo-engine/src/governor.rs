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

    /// Re-targets the budget (e.g. the host learned how much RAM the
    /// machine has, or the OS signalled memory pressure). Returns the
    /// entries to drop, LRU first, so the new budget is respected.
    #[must_use]
    pub fn set_budget(&mut self, budget_bytes: usize) -> Vec<EntryId> {
        self.budget = budget_bytes;
        let mut victims = Vec::new();
        while self.used > self.budget && !self.entries.is_empty() {
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
        victims
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
    ///
    /// `pinned` entries are never evicted: a scene being assembled holds ids
    /// it fetched moments ago, and evicting one of those to make room for the
    /// next would leave the scene pointing at a freed frame — under a tight
    /// budget that was a panic per render. Pinning may leave the cache
    /// transiently over budget; the next unpinned admit pays it back.
    #[must_use]
    pub fn admit(&mut self, id: EntryId, bytes: usize, pinned: &[EntryId]) -> Vec<EntryId> {
        self.tick += 1;
        if let Some(old) = self.entries.remove(&id) {
            self.used -= old.bytes;
        }
        let mut victims = Vec::new();
        while self.used + bytes > self.budget {
            let candidate = self
                .entries
                .iter()
                .filter(|(eid, _)| !pinned.contains(eid))
                .min_by_key(|(_, e)| e.last_used)
                .map(|(&eid, _)| eid);
            let Some(victim) = candidate else { break };
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
        assert!(g.admit(1, 40, &[]).is_empty());
        assert!(g.admit(2, 40, &[]).is_empty());
        g.touch(1); // 2 is now least-recently-used
        let victims = g.admit(3, 40, &[]);
        assert_eq!(victims, vec![2]);
        assert_eq!(g.used(), 80);
        assert_eq!(g.evictions(), 1);
    }

    #[test]
    fn oversized_entry_admitted_alone() {
        let mut g = MemoryGovernor::new(100);
        assert!(g.admit(1, 60, &[]).is_empty());
        let victims = g.admit(2, 500, &[]);
        assert_eq!(victims, vec![1]);
        assert_eq!(g.used(), 500);
    }

    #[test]
    fn re_admit_replaces_cost() {
        let mut g = MemoryGovernor::new(100);
        assert!(g.admit(1, 90, &[]).is_empty());
        assert!(g.admit(1, 30, &[]).is_empty());
        assert_eq!(g.used(), 30);
    }

    #[test]
    fn shrinking_budget_evicts_lru_immediately() {
        let mut g = MemoryGovernor::new(300);
        assert!(g.admit(1, 100, &[]).is_empty());
        assert!(g.admit(2, 100, &[]).is_empty());
        assert!(g.admit(3, 100, &[]).is_empty());
        g.touch(3);
        g.touch(2);
        let victims = g.set_budget(200);
        assert_eq!(victims, vec![1], "least-recently-used goes first");
        assert_eq!(g.used(), 200);
        assert_eq!(g.budget(), 200);
        // Growing the budget evicts nothing.
        assert!(g.set_budget(1_000).is_empty());
    }

    #[test]
    fn remove_frees_bytes() {
        let mut g = MemoryGovernor::new(100);
        let _ = g.admit(1, 70, &[]);
        g.remove(1);
        assert_eq!(g.used(), 0);
        assert!(g.admit(2, 100, &[]).is_empty());
    }

    #[test]
    fn pinned_entries_survive_even_over_budget() {
        let mut g = MemoryGovernor::new(100);
        assert!(g.admit(1, 60, &[]).is_empty());
        assert!(
            g.admit(2, 60, &[1]).is_empty(),
            "1 is pinned: nothing to evict"
        );
        assert!(g.used() > g.budget(), "transiently over budget by design");
        // With the pin gone, the next admit pays the whole debt back, LRU
        // first — both old entries go, because 60 is all the budget holds.
        let victims = g.admit(3, 60, &[]);
        assert_eq!(victims, vec![1, 2]);
        assert_eq!(g.used(), 60);
    }
}
