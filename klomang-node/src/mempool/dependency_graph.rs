//! Transaction Dependency Graph and Conflict Set Partitioning
//!
//! Manages transaction dependencies and propagates conflict status to dependent transactions.
//! Prevents orphaned children from being included if their parent is in conflict.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use parking_lot::Mutex;

use super::advanced_conflicts::TxHash;

/// Tracks the dependency relationship between transactions
#[derive(Clone, Debug)]
pub struct TransactionDependency {
    /// Parents (transactions this TX depends on)
    pub parents: HashSet<TxHash>,

    /// Children (transactions that depend on this TX)
    pub children: HashSet<TxHash>,

    /// Conflict status
    pub in_conflict: bool,

    /// If in conflict, reason for the conflict
    pub conflict_reason: Option<String>,
}

impl TransactionDependency {
    pub fn new(_tx_hash: &TxHash) -> Self {
        Self {
            parents: HashSet::new(),
            children: HashSet::new(),
            in_conflict: false,
            conflict_reason: None,
        }
    }
}

/// Conflict set partition - group of related transactions
#[derive(Clone, Debug)]
pub struct ConflictPartition {
    /// All transactions in this partition
    pub transactions: HashSet<TxHash>,

    /// Is this partition in conflict?
    pub in_conflict: bool,

    /// Reason for conflict
    pub conflict_reason: Option<String>,

    /// ID of this partition
    pub id: u64,
}

impl ConflictPartition {
    pub fn new(id: u64) -> Self {
        Self {
            transactions: HashSet::new(),
            in_conflict: false,
            conflict_reason: None,
            id,
        }
    }

    pub fn mark_conflict(&mut self, reason: String) {
        self.in_conflict = true;
        self.conflict_reason = Some(reason);
    }
}

/// Statistics for dependency graph operations
#[derive(Clone, Debug, Default)]
pub struct DependencyGraphStats {
    pub total_dependencies: u64,
    pub total_partitions: u64,
    pub conflict_propagations: u64,
    pub affected_transactions: u64,
}

/// Dependency graph engine for transaction relationships
pub struct DependencyGraph {
    /// Transaction ID → Dependencies
    dependencies: Arc<Mutex<HashMap<TxHash, TransactionDependency>>>,

    /// Partition assignments
    partitions: Arc<Mutex<HashMap<TxHash, u64>>>,

    /// All partitions by ID
    partition_data: Arc<Mutex<HashMap<u64, ConflictPartition>>>,

    /// Statistics
    stats: Arc<Mutex<DependencyGraphStats>>,

    /// Next partition ID
    next_partition_id: Arc<Mutex<u64>>,
}

impl DependencyGraph {
    /// Create new dependency graph
    pub fn new() -> Self {
        Self {
            dependencies: Arc::new(Mutex::new(HashMap::new())),
            partitions: Arc::new(Mutex::new(HashMap::new())),
            partition_data: Arc::new(Mutex::new(HashMap::new())),
            stats: Arc::new(Mutex::new(DependencyGraphStats::default())),
            next_partition_id: Arc::new(Mutex::new(1)),
        }
    }

    /// Register a transaction in the graph
    pub fn register_transaction(&self, tx_hash: &TxHash) {
        let mut deps = self.dependencies.lock();
        if !deps.contains_key(tx_hash) {
            deps.insert(tx_hash.clone(), TransactionDependency::new(tx_hash));

            // Create new partition for this transaction
            let mut next_id = self.next_partition_id.lock();
            let partition_id = *next_id;
            *next_id += 1;

            drop(next_id);

            let mut partitions = self.partitions.lock();
            partitions.insert(tx_hash.clone(), partition_id);

            let mut partition_data = self.partition_data.lock();
            let mut partition = ConflictPartition::new(partition_id);
            partition.transactions.insert(tx_hash.clone());
            partition_data.insert(partition_id, partition);

            let mut stats = self.stats.lock();
            stats.total_partitions += 1;
        }
    }

    /// Add dependency: child depends on parent
    pub fn add_dependency(&self, child: &TxHash, parent: &TxHash) -> Result<(), String> {
        let mut deps = self.dependencies.lock();

        // Ensure both transactions exist
        if !deps.contains_key(child) {
            drop(deps);
            self.register_transaction(child);
            deps = self.dependencies.lock();
        }

        if !deps.contains_key(parent) {
            drop(deps);
            self.register_transaction(parent);
            deps = self.dependencies.lock();
        }

        // Add parent to child's parents
        if let Some(child_dep) = deps.get_mut(child) {
            child_dep.parents.insert(parent.clone());
        }

        // Add child to parent's children
        if let Some(parent_dep) = deps.get_mut(parent) {
            parent_dep.children.insert(child.clone());
        }

        // Merge partitions: child joins parent's partition
        self.merge_partitions(child, parent)?;

        let mut stats = self.stats.lock();
        stats.total_dependencies += 1;

        Ok(())
    }

    /// Merge two partitions (child joins parent's partition)
    fn merge_partitions(&self, child: &TxHash, parent: &TxHash) -> Result<(), String> {
        let partitions = self.partitions.lock();

        let child_partition_id = *partitions.get(child).ok_or("Child not in partitions")?;
        let parent_partition_id = *partitions.get(parent).ok_or("Parent not in partitions")?;

        drop(partitions);

        if child_partition_id == parent_partition_id {
            return Ok(());
        }

        // Get all transactions from child's partition
        let mut partition_data = self.partition_data.lock();
        let child_partition = partition_data
            .get(&child_partition_id)
            .ok_or("Child partition not found")?
            .clone();

        let to_merge: Vec<TxHash> = child_partition.transactions.iter().cloned().collect();

        // Move all to parent's partition
        if let Some(parent_partition) = partition_data.get_mut(&parent_partition_id) {
            for tx in &to_merge {
                parent_partition.transactions.insert(tx.clone());
            }

            // Propagate conflict status if parent partition is in conflict
            if child_partition.in_conflict {
                parent_partition.in_conflict = true;
                if let Some(reason) = &child_partition.conflict_reason {
                    parent_partition.conflict_reason = Some(reason.clone());
                }
            }
        }

        drop(partition_data);

        // Update partition assignments
        let mut partitions = self.partitions.lock();
        for tx in to_merge {
            partitions.insert(tx, parent_partition_id);
        }

        Ok(())
    }

    /// Mark a transaction and its descendants as in conflict
    pub fn mark_conflict(&self, tx_hash: &TxHash, reason: String) -> Result<Vec<TxHash>, String> {
        let mut affected = Vec::new();

        let partitions = self.partitions.lock();
        let partition_id = *partitions.get(tx_hash).ok_or("Transaction not in graph")?;
        drop(partitions);

        // Mark entire partition as in conflict
        let mut partition_data = self.partition_data.lock();
        if let Some(partition) = partition_data.get_mut(&partition_id) {
            partition.mark_conflict(reason.clone());
            affected = partition.transactions.iter().cloned().collect();
        }

        drop(partition_data);

        let mut deps = self.dependencies.lock();
        for tx in &affected {
            if let Some(dep) = deps.get_mut(tx) {
                dep.in_conflict = true;
                dep.conflict_reason = Some(reason.clone());
            }
        }

        let mut stats = self.stats.lock();
        stats.conflict_propagations += 1;
        stats.affected_transactions += affected.len() as u64;

        Ok(affected)
    }

    /// Get all dependent transactions (children)
    pub fn get_dependents(&self, tx_hash: &TxHash) -> Vec<TxHash> {
        let deps = self.dependencies.lock();
        deps.get(tx_hash)
            .map(|dep| dep.children.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Get all parent transactions
    pub fn get_parents(&self, tx_hash: &TxHash) -> Vec<TxHash> {
        let deps = self.dependencies.lock();
        deps.get(tx_hash)
            .map(|dep| dep.parents.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Get conflict status of transaction
    pub fn is_in_conflict(&self, tx_hash: &TxHash) -> bool {
        let deps = self.dependencies.lock();
        deps.get(tx_hash)
            .map(|dep| dep.in_conflict)
            .unwrap_or(false)
    }

    /// Get conflict reason
    pub fn get_conflict_reason(&self, tx_hash: &TxHash) -> Option<String> {
        let deps = self.dependencies.lock();
        deps.get(tx_hash)
            .and_then(|dep| dep.conflict_reason.clone())
    }

    /// Get all transactions in same partition (connected component)
    pub fn get_partition_members(&self, tx_hash: &TxHash) -> Vec<TxHash> {
        let partitions = self.partitions.lock();
        let partition_id = match partitions.get(tx_hash) {
            Some(&id) => id,
            None => return Vec::new(),
        };

        drop(partitions);

        let partition_data = self.partition_data.lock();
        partition_data
            .get(&partition_id)
            .map(|p| p.transactions.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Find all affected transactions downstream from a transaction
    pub fn find_affected_downstream(&self, tx_hash: &TxHash) -> Vec<TxHash> {
        let mut affected = Vec::new();
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();

        queue.push_back(tx_hash.clone());
        visited.insert(tx_hash.clone());

        while let Some(current) = queue.pop_front() {
            affected.push(current.clone());

            let children = self.get_dependents(&current);
            for child in children {
                if !visited.contains(&child) {
                    visited.insert(child.clone());
                    queue.push_back(child);
                }
            }
        }

        affected
    }

    /// Remove transaction from graph
    pub fn remove_transaction(&self, tx_hash: &TxHash) -> Result<(), String> {
        let mut deps = self.dependencies.lock();

        // Collect parents and children first to avoid borrow issues
        let (parents, children) = if let Some(dep) = deps.get(tx_hash) {
            (
                dep.parents.iter().cloned().collect::<Vec<_>>(),
                dep.children.iter().cloned().collect::<Vec<_>>(),
            )
        } else {
            (vec![], vec![])
        };

        // Update parents: remove this from their children
        for parent in &parents {
            if let Some(parent_dep) = deps.get_mut(parent) {
                parent_dep.children.remove(tx_hash);
            }
        }

        // Update children: remove this from their parents
        for child in &children {
            if let Some(child_dep) = deps.get_mut(child) {
                child_dep.parents.remove(tx_hash);
            }
        }

        deps.remove(tx_hash);

        let mut partitions = self.partitions.lock();
        partitions.remove(tx_hash);

        Ok(())
    }

    /// Get statistics
    pub fn get_stats(&self) -> DependencyGraphStats {
        self.stats.lock().clone()
    }

    /// Get partition data
    pub fn get_partition(&self, tx_hash: &TxHash) -> Option<ConflictPartition> {
        let partitions = self.partitions.lock();
        let partition_id = partitions.get(tx_hash)?;

        let partition_data = self.partition_data.lock();
        partition_data.get(partition_id).cloned()
    }

    /// Clear all data
    pub fn clear(&self) {
        self.dependencies.lock().clear();
        self.partitions.lock().clear();
        self.partition_data.lock().clear();
        let mut stats = self.stats.lock();
        *stats = DependencyGraphStats::default();
        let mut next_id = self.next_partition_id.lock();
        *next_id = 1;
    }

    pub fn len(&self) -> usize {
        self.dependencies.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.dependencies.lock().is_empty()
    }
}

impl Default for DependencyGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DependencyGraph {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx_hash(id: u8) -> TxHash {
        TxHash::new(vec![id; 32])
    }

    #[test]
    fn test_dependency_graph_creation() {
        let graph = DependencyGraph::new();
        assert!(graph.is_empty());
    }

    #[test]
    fn test_register_transaction() {
        let graph = DependencyGraph::new();
        let tx = tx_hash(1);

        graph.register_transaction(&tx);
        assert_eq!(graph.len(), 1);
    }

    #[test]
    fn test_add_dependency() {
        let graph = DependencyGraph::new();
        let parent = tx_hash(1);
        let child = tx_hash(2);

        graph.register_transaction(&parent);
        graph.register_transaction(&child);

        let result = graph.add_dependency(&child, &parent);
        assert!(result.is_ok());

        let children = graph.get_dependents(&parent);
        assert!(children.contains(&child));

        let parents = graph.get_parents(&child);
        assert!(parents.contains(&parent));
    }

    #[test]
    fn test_mark_conflict() {
        let graph = DependencyGraph::new();
        let parent = tx_hash(1);
        let child = tx_hash(2);

        graph.register_transaction(&parent);
        graph.register_transaction(&child);
        graph.add_dependency(&child, &parent).ok();

        let affected = graph.mark_conflict(&parent, "Test conflict".to_string());
        assert!(affected.is_ok());

        assert!(graph.is_in_conflict(&parent));
        assert!(graph.is_in_conflict(&child));
    }

    #[test]
    fn test_partition_merging() {
        let graph = DependencyGraph::new();
        let tx1 = tx_hash(1);
        let tx2 = tx_hash(2);

        graph.register_transaction(&tx1);
        graph.register_transaction(&tx2);

        // Initially in different partitions
        let partition1 = graph.get_partition(&tx1);
        let partition2 = graph.get_partition(&tx2);
        assert_ne!(
            partition1.as_ref().map(|p| p.id),
            partition2.as_ref().map(|p| p.id)
        );

        // Add dependency - should merge partitions
        graph.add_dependency(&tx2, &tx1).ok();

        let new_partition2 = graph.get_partition(&tx2);
        assert_eq!(
            partition1.as_ref().map(|p| p.id),
            new_partition2.as_ref().map(|p| p.id)
        );
    }

    #[test]
    fn test_find_affected_downstream() {
        let graph = DependencyGraph::new();

        let tx1 = tx_hash(1);
        let tx2 = tx_hash(2);
        let tx3 = tx_hash(3);

        graph.register_transaction(&tx1);
        graph.register_transaction(&tx2);
        graph.register_transaction(&tx3);

        graph.add_dependency(&tx2, &tx1).ok();
        graph.add_dependency(&tx3, &tx2).ok();

        let affected = graph.find_affected_downstream(&tx1);
        assert_eq!(affected.len(), 3);
        assert!(affected.contains(&tx1));
        assert!(affected.contains(&tx2));
        assert!(affected.contains(&tx3));
    }

    #[test]
    fn test_remove_transaction() {
        let graph = DependencyGraph::new();
        let tx = tx_hash(1);

        graph.register_transaction(&tx);
        assert_eq!(graph.len(), 1);

        graph.remove_transaction(&tx).ok();
        assert_eq!(graph.len(), 0);
    }

    #[test]
    fn test_conflict_reason() {
        let graph = DependencyGraph::new();
        let tx = tx_hash(1);

        graph.register_transaction(&tx);
        graph
            .mark_conflict(&tx, "Double spend detected".to_string())
            .ok();

        let reason = graph.get_conflict_reason(&tx);
        assert_eq!(reason, Some("Double spend detected".to_string()));
    }
}
