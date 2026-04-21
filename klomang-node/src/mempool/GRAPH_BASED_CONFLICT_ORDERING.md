# Graph-Based Conflict Detection & Deterministic Ordering System

## Overview

This system provides:
- **Conflict Detection**: Instant double-spend detection through graph traversal
- **Deterministic Ordering**: Exactly reproducible transaction ordering across all validator nodes
- **Parallel Execution**: DSU-based grouping for non-conflicting transaction parallel processing
- **UTXO Validation**: Integration with on-chain state for UTXO conflict management

## Architecture

### 1. Core Components

#### GraphConflictOrderingEngine
The main engine that manages:
- **UTXO Index**: Maps each UTXO to claiming transactions
- **Conflict Graph**: Tracks conflict relationships between transactions
- **Dependency Graph**: Maintains parent-child relationships for cascade validation
- **Priority Scoring**: Combines fee density and age for transaction ranking

#### DisjointSetUnion (DSU)
Used for efficient grouping of non-conflicting transactions:
- Path compression optimization
- Union by rank strategy
- O(log n) amortized time complexity

#### TransactionNode
Stores transaction information:
- Fee and size for fee density calculation
- Arrival time for age-based priority
- In/out degree for topological sorting
- Input/output counts for UTXO tracking

### 2. Conflict Detection Algorithm

```
For each new transaction:
1. Input Validation: Check each input against UTXO index
2. Conflict Detection: Find all existing claimants of same UTXOs
3. Bidirectional Link: Register conflict in both directions
4. Transitive Analysis: Mark dependent transactions as affected
5. Cache Invalidation: Clear cached ordering
```

Time Complexity: O(n*m) where n=inputs, m=existing claimants
Space Complexity: O(T) where T=total transactions

### 3. Deterministic Canonical Ordering

The ordering algorithm ensures deterministic results through:

```
1. Topological Sort (Kahn's Algorithm):
   - Calculate in-degrees for all transactions
   - Process nodes with in-degree=0
   - Maintain strict ordering within each layer

2. Priority Scoring:
   Fee Score = Fee / SizeBytes
   Age Score = (CurrentTime - ArrivalTime) / 1000
   Priority = FeeScore × fee_weight + AgeScore × age_weight
   
3. Tie-breaking:
   - Primary: Priority score (higher wins)
   - Secondary: Hash comparison (lexicographic)
   - Applied consistently across all nodes

4. Parallel Layers:
   - Each topological layer can be parallelized
   - All transactions in layer_N have no dependencies on layer_M (M>N)
```

Example Ordering:
```
Input: [TxA(fee=5,age=0), TxB(fee=10,age=5), TxC(fee=10,age=2)]
Priority Scores (weights: 0.7 fee, 0.3 age):
  TxB: 10/100*0.7 + 5*0.3 = 0.204
  TxC: 10/100*0.7 + 2*0.3 = 0.164  
  TxA: 5/100*0.7 + 0*0.3 = 0.035

Output: [TxB, TxC, TxA]
Parallel Groups: [[TxB, TxC], [TxA]]  (if TxB-TxC non-dependent)
```

### 4. Integration with TransactionPool

The integration layer (`graph_conflict_ordering_integration.rs`) provides:

1. **Conflict Registration**: Automatic conflict detection on `add_transaction()`
2. **UTXO Validation**: Verify inputs against on-chain state
3. **Block Building**: Construct canonical blocks respecting conflicts
4. **Cascade Removal**: Remove dependent transactions when parent is removed
5. **Parallel Groups**: Get transactions for parallel block validation

### 5. UTXO Validation Integration

```
Validation Points:
├─ Mempool Level: Detect conflicts between pool transactions
├─ State Level: Check UTXOs against on-chain state
├─ Dependency Level: Validate parent existence before accepting child
└─ Consensus Level: Ensure identical ordering across validators
```

## Usage Examples

### Basic Integration

```rust
use klomang_node::mempool::graph_conflict_ordering_integration::{
    ConflictOrderingIntegration,
    ConflictOrderingIntegrationConfig,
};

// Create integration with config
let config = ConflictOrderingIntegrationConfig::default();
let integration = ConflictOrderingIntegration::new(config, kv_store);

// Register transaction
let result = integration.register_transaction(
    &transaction,
    tx_hash,
    fee,
    arrival_time_ms,
)?;

if result.has_double_spend {
    println!("Double spend detected!");
}
```

### Block Building

```rust
// Build canonical block
let block_result = integration.build_block_canonical(1_000_000)?;

println!("Block transactions: {}", block_result.transactions.len());
println!("Total fees: {}", block_result.total_fees);
println!("Parallel layers: {}", block_result.parallel_layers.len());
```

### Parallel Validation

```rust
// Get validation groups for parallel processing
let groups = integration.get_parallel_validation_groups()?;

for (layer_idx, layer) in groups.iter().enumerate() {
    // Each layer can be validated in parallel
    println!("Layer {}: {} transactions", layer_idx, layer.len());
}
```

## Performance Characteristics

| Operation | Time Complexity | Space | Notes |
|-----------|-----------------|-------|-------|
| Register TX | O(I × C) | O(T) | I=inputs, C=avg conflicts |
| Canonical Order | O(T log T) | O(T) | Cached result |
| Parallel Groups | O(T+E) | O(T) | E=edges in conflict graph |
| Remove Cascade | O(D + E) | O(D) | D=dependents |

Where T=transactions, I=inputs, C=conflicts, D=dependents, E=edges

## Consensus Safety

The system ensures consensus safety through:

1. **Deterministic Output**: Same input always produces same ordering
2. **Proof Verification**: Hash-based ordering guarantees
3. **Conflict Resolution**: Deterministic RBF apply all nodes
4. **State Consistency**: Cached ordering invalidation on mutations
5. **Peer Validation**: `validate_against_peer()` checks ordering equality

## Testing

Run tests with:
```bash
cargo test -p klomang-node graph_conflict_ordering
cargo test -p klomang-node graph_conflict_ordering_integration
```

## Future Enhancements

1. **Petgraph Integration**: Use petgraph library for more efficient graph algorithms
2. **Batch Processing**: Process multiple transactions together for efficiency  
3. **Cycle Detection**: Implement DFS-based cycle detection for RBF safety
4. **Memory Optimization**: Use arena allocation for large graphs
5. **Persistence**: Serialize/deserialize ordering snapshots to disk

## Files

- `graph_conflict_ordering.rs`: Core engine
- `graph_conflict_ordering_integration.rs`: Integration layer
- `GRAPH_BASED_CONFLICT_ORDERING.md`: This documentation
