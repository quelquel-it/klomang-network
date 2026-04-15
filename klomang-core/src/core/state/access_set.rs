use std::collections::HashSet;

/// Represents the set of keys accessed by a transaction
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AccessSet {
    pub read_set: HashSet<[u8; 32]>,
    pub write_set: HashSet<[u8; 32]>,
}

impl AccessSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn has_conflict(&self, other: &AccessSet) -> bool {
        // Conflict if any write set overlaps with any read or write set of other
        !self.write_set.is_disjoint(&other.read_set) ||
        !self.write_set.is_disjoint(&other.write_set) ||
        !other.write_set.is_disjoint(&self.read_set)
    }

    pub fn merge(&mut self, other: &AccessSet) {
        self.read_set.extend(&other.read_set);
        self.write_set.extend(&other.write_set);
    }
}