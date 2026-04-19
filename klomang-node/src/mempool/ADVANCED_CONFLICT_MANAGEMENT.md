# Advanced Transaction Conflict Management System

## Overview

The Advanced Transaction Conflict Management System provides deterministic double-spend detection and resolution for the Klomang blockchain mempool. It prevents invalid transactions from being included in blocks while maintaining consistency across all network nodes through deterministic conflict resolution rules.

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│        AdvancedTransactionManager (Main Orchestrator)            │
└─────────────────────────────────────────────────────────────────┘
                            │
        ┌───────────────────┼───────────────────┐
        │                   │                   │
        ▼                   ▼                   ▼
   ConflictMap      DependencyGraph      TransactionPool
   ────────────     ────────────────     ───────────────
   • Tracks         • Dependency         • Stores TX
     OutPoints        relationships       • Fee indexing
   • Detects        • Partition          • Status mgmt
     conflicts        creation
   • Resolves       • Conflict
     conflicts        propagation
```

## Core Components

### 1. ConflictMap - Multi-Input Conflict Tracking

**Purpose**: Tracks which transactions claim each UTXO output to detect double-spending attempts.

**Data Structure**:
```rust
pub struct ConflictMap {
    // OutPoint → Set of TxHashes claiming it
    conflicts: HashMap<OutPoint, HashSet<TxHash>>,
    
    // Transaction ID → Arrival time (for timestamp tie-breaking)
    arrival_times: HashMap<TxHash, u64>,
    
    // Statistics
    stats: ConflictStats,
}
```

**Key Operations**:
- `register_transaction()`: Detect if inputs conflict with existing transactions
- `resolve_conflict()`: Deterministically choose winner between conflicting transactions
- `remove_transaction()`: Clean up after transaction is evicted/confirmed
- `get_conflicted_outpoints()`: Query which UTXOs have conflicts

**Conflict Detection Algorithm**:
```
FOR each input in new_transaction:
    outpoint = (prev_tx_hash, output_index)
    IF outpoint exists in conflicts:
        IF other_tx claiming same outpoint:
            RETURN DirectConflict(tx_a, tx_b, outpoint)
    ELSE:
        Create new entry for outpoint
    
    Add new_tx to claimants for outpoint
RETURN NoConflict
```

### 2. Deterministic Resolution Rules

The system uses a three-tier deterministic algorithm to resolve conflicts, ensuring all network nodes reach the same decision:

```
Rule 1 - Fee Rate Priority (HIGHEST WEIGHT)
┌─────────────────────────────────────────────┐
│ TX A: 1000 sat / 200 bytes = 5.0 sat/byte   │
├─────────────────────────────────────────────┤
│ TX B: 2000 sat / 100 bytes = 20.0 sat/byte  │ ← WINNER
├─────────────────────────────────────────────┤
│ Decision: Higher fee rate wins               │
│ (prevents fee-based eviction attacks)        │
└─────────────────────────────────────────────┘

Rule 2 - Timestamp (MEDIUM WEIGHT)
┌─────────────────────────────────────────────┐
│ TX A: 5.0 sat/byte, arrived at t=1000      │ ← WINNER
├─────────────────────────────────────────────┤
│ TX B: 5.0 sat/byte, arrived at t=2000      │
├─────────────────────────────────────────────┤
│ Decision: Earlier arrival wins               │
│ (prevents censorship by delaying transactions)│
└─────────────────────────────────────────────┘

Rule 3 - Lexicographical Hash (LOWEST WEIGHT)
┌─────────────────────────────────────────────┐
│ TX A: 5.0 sat/byte, same timestamp          │
│       hash = 0x01...                        │ ← WINNER
├─────────────────────────────────────────────┤
│ TX B: 5.0 sat/byte, same timestamp          │
│       hash = 0xFF...                        │
├─────────────────────────────────────────────┤
│ Decision: Lexicographically smaller hash    │
│ (deterministic tie-breaker)                 │
└─────────────────────────────────────────────┘
```

**Resolution Code**:
```rust
pub fn resolve_conflict(
    &self,
    tx_a: &Transaction,
    tx_b: &Transaction,
    fee_a: u64, fee_b: u64,
    size_a: usize, size_b: usize,
) -> ResolutionResult {
    let rate_a = fee_a as f64 / size_a as f64;
    let rate_b = fee_b as f64 / size_b as f64;
    
    if (rate_a - rate_b).abs() > EPSILON {
        // Rule 1: Fee rate differs
        return if rate_a > rate_b { A_WINS } else { B_WINS };
    }
    
    let time_a = arrival_times.get(tx_a).unwrap_or(u64::MAX);
    let time_b = arrival_times.get(tx_b).unwrap_or(u64::MAX);
    
    if time_a != time_b {
        // Rule 2: Timestamps differ
        return if time_a < time_b { A_WINS } else { B_WINS };
    }
    
    // Rule 3: Hash comparison
    if tx_a.hash < tx_b.hash { A_WINS } else { B_WINS }
}
```

### 3. DependencyGraph - Conflict Set Partitioning

**Purpose**: Tracks transaction dependencies and propagates conflict status to dependent transactions.

**Data Structure**:
```rust
pub struct DependencyGraph {
    // TX → Dependency info
    dependencies: HashMap<TxHash, TransactionDependency>,
    
    // TX → Partition ID
    partitions: HashMap<TxHash, u64>,
    
    // Partition ID → Members
    partition_data: HashMap<u64, ConflictPartition>,
}

pub struct TransactionDependency {
    pub parents: HashSet<TxHash>,      // TX this depends on
    pub children: HashSet<TxHash>,     // TX that depend on this
    pub in_conflict: bool,             // If part of conflicted set
    pub conflict_reason: Option<String>,
}
```

**Partition Behavior**:
```
Initial State:
TX-A (partition 1)    TX-B (partition 2)
│                     │
├─ TX-C               └─ TX-D

After add_dependency(TX-B → TX-A):
Merged to same partition:
TX-A
├─ TX-C
├─ TX-B (merged in)
└─ TX-D (via TX-B)

All in same partition (3)
```

**Conflict Propagation**:
```
mark_conflict(TX-A, reason="Double-spend detected")

TX-A (CONFLICT)
├─ TX-C (automatically CONFLICT)
├─ TX-B (merged via dependency)
│  └─ TX-D (now CONFLICT via chain)

All affected = [TX-A, TX-B, TX-C, TX-D]
Status: in_conflict = true for all
```

### 4. AdvancedTransactionManager - Integration Layer

**Purpose**: Coordinates ConflictMap, DependencyGraph, and TransactionPool to manage complete transaction lifecycle.

**Workflow**:
```
add_transaction(tx, fee, size)
│
├─ [1] Register in dependency graph
│      → Create partition
│
├─ [2] Register in conflict map
│      → Check for input conflicts
│
├─ [3] Route based on result
│      ├─ NO_CONFLICT: add_to_pool_safe()
│      │  ├─ Verify UTXOs exist in blockchain
│      │  ├─ Validate signatures
│      │  └─ Insert to pool
│      │
│      └─ DIRECT_CONFLICT: resolve_conflict()
│         ├─ Get existing transaction from pool
│         ├─ Compare fee rates deterministically
│         ├─ If new TX wins:
│         │  ├─ Mark old as conflict
│         │  ├─ Remove old from pool/graph
│         │  └─ Add new to pool
│         └─ If old TX wins:
│            └─ Reject new transaction
│
└─ Return TransactionAdditionResult
```

## Deterministic Operation Guarantees

### 1. Same Decision Across Nodes

All nodes running this system will make identical decisions for the same inputs:

```
Node A receives TX-Conflicted-A at timestamp T1:    "REJECT"
Node B receives TX-Conflicted-A at timestamp T1:    "REJECT"
Node C receives TX-Conflicted-A at timestamp T1:    "REJECT"

Decision is based on:
- Fee rate (deterministic calculation)
- Absolute timestamp (T1, not relative)
- Transaction hash (deterministic value)

Pure function: f(TX, fee, size, timestamp) → KEEP or EVICT
```

### 2. No Randomness

All conflict resolution uses:
- Arithmetic operations (fee/size)
- Timestamp comparison
- Byte comparison (lexicographical)

No randomness means identical fork-free results everywhere.

### 3. Timestamp Stability

The timestamp is recorded at first arrival in ConflictMap:
- Used only for tie-breaking when fee rates identical
- Prevents attackers from delaying transactions to change outcomes
- Network-synchronized timestamps ensure consistency

## Prevention Mechanisms

### Double-Spending Prevention

```
Attack: Alice creates two transactions spending same UTXO
TX-1: Alice → Bob (10 BTC)  [fee: 5000 sat, 250 bytes = 20 sat/byte]
TX-2: Alice → Carol (10 BTC) [fee: 1000 sat, 250 bytes = 4 sat/byte]

Result:
1. TX-1 arrives first: registered in ConflictMap
2. TX-2 arrives: registers inputs
   - UTXO[0] already claimed by TX-1
   - Detects DIRECT_CONFLICT
3. Resolve TX-1 vs TX-2:
   - Fee rate TX-1: 20 > TX-2: 4
   - TX-1 WINS → TX-2 EVICTED
4. Carol never receives funds, network achieves consensus
```

### Fee-Rate Gaming Prevention

```
Attack: Attacker evicts legitimate TX with tiny fee increase
Legitimate: 1 BTC fee / 1000 bytes = 0.00001 BTC/byte
Attack:     1.001 BTC fee / 1000 bytes = 0.001001 BTC/byte

Protection: Fee rate uses floating-point with epsilon:
if (fee_rate_a - fee_rate_b).abs() > EPSILON (0.01)
    → Clear winner

This prevents dust-sized fee increases from evicting transactions
```

### Orphaned Transaction Prevention

```
Dependency Chain:
Parent-TX (needs confirmation)
│
└─ Child-TX (pays from Parent output)

Attack: Exclude Parent-TX from chain to orphan Child-TX

Results:
1. Parent-TX gets evicted (conflicted)
2. Child-TX marked as in-conflict automatically
   - Via DependencyGraph partition
3. Child-TX won't be selected for block building
4. No orphaned UTXOs in blockchain state
```

## Integration with Storage Layer

### UTXO Verification

```rust
// Before accepting transaction, verify:
FOR each input in transaction:
    outpoint = (prev_tx_hash, input_index)
    
    // Check on-chain UTXO exists
    IF NOT kv_store.utxo_exists(outpoint):
        REJECT: Input not in UTXO set
    
    // Check not already spent
    IF kv_store.is_spent(outpoint):
        REJECT: Double-spend attempt (on-chain)
```

### Signature Verification

```rust
// Validate transaction signatures against blockchain state
FOR each input in transaction:
    prev_output = get_previous_output(input.prev_tx, input.index)
    
    IF NOT verify_signature(input.signature, prev_output.script_pubkey):
        REJECT: Invalid signature
```

## Statistics and Monitoring

### ConflictStats

```rust
pub struct ConflictStats {
    pub total_conflicts_detected: u64,  // All conflicts seen
    pub total_resolutions: u64,         // Resolution attempts
    pub total_evictions: u64,           // TXs evicted
    pub direct_conflicts: u64,          // Input conflicts
    pub indirect_conflicts: u64,        // Dependency conflicts
}
```

### ConflictAnalysis

```
Mempool Conflict Report:
├─ Total conflicts detected: 42
├─ Conflicted outpoints: 5
├─ Affected transactions: 18
├─ Total resolutions: 12
├─ Total evictions: 12
├─ Partition count: 7
└─ Conflict propagations: 3
```

## Performance Characteristics

| Operation | Complexity | Notes |
|-----------|-----------|-------|
| `register_transaction()` | O(inputs) | Linear in TX inputs |
| `resolve_conflict()` | O(1) | Constant-time comparison |
| `mark_conflict()` | O(partition) | Affects all in partition |
| `find_affected_downstream()` | O(V+E) | V=TXs, E=dependencies |
| `get_conflicted_outpoints()` | O(n) | n=total outpoints |

## Thread Safety

- **ConflictMap**: `parking_lot::Mutex` protecting HashMap
- **DependencyGraph**: `parking_lot::Mutex` for dependencies
- **AdvancedTransactionManager**: Lock serialization ensures atomic operations

## Error Handling

### ManagerError Types

```rust
pub enum ManagerError {
    ConflictDetected { msg: String },      // Conflict occurred
    DependencyError { msg: String },       // Graph operation failed
    StorageError { msg: String },          // KvStore problem
    InvalidTransaction { msg: String },    // Validation failed
    ResolutionFailed { msg: String },      // Resolution error
}
```

### Graceful Degradation

```
Scenario: Storage unavailable for UTXO verification

Behavior:
1. add_transaction() calls verify_utxo_exists()
2. KvStore returns error
3. Converted to ManagerError::StorageError
4. Transaction rejected with reason
5. Pool state remains consistent
```

## Testing Coverage

### Included Tests

1. **Triple Conflict Detection** - 3+ transactions on same input
2. **Timestamp Resolution** - Fee-rate tie-breaking
3. **Lexicographical Resolution** - Final tie-breaker
4. **Dependency Propagation** - Conflict cascade through chain
5. **Multi-branch Dependencies** - Tree-structured conflicts
6. **Complex Multi-input** - Multiple UTXO conflicts
7. **Partition Merging** - Dependency graph consolidation
8. **Affected Downstream** - Cascade calculation
9. **Orphaned Transactions** - Parent-child conflicts
10. **Conflict Analysis** - Statistics accuracy

## Production Readiness

✓ No `todo!()` or placeholder implementations
✓ Comprehensive error handling throughout
✓ Thread-safe with parking_lot mutexes
✓ Deterministic results across nodes
✓ Full integration with mempool
✓ Storage layer verification
✓ Performance optimized for high throughput

## Usage Example

```rust
// Initialize components
let kv_store = Arc::new(KvStore::new());
let conflict_map = Arc::new(ConflictMap::new(kv_store.clone()));
let graph = Arc::new(DependencyGraph::new());
let pool = Arc::new(TransactionPool::new(...));

// Create manager
let manager = AdvancedTransactionManager::new(
    conflict_map, graph, pool, kv_store,
);

// Add transaction with automatic conflict handling
match manager.add_transaction(tx, fee, size) {
    Ok(result) => {
        println!("Added: {}", result.added);
        println!("Evicted: {:?}", result.evicted);
    }
    Err(e) => println!("Rejected: {}", e),
}

// Analyze conflicts
let analysis = manager.analyze_conflicts()?;
println!("Affected TX: {}", analysis.affected_transactions);
```

## Future Enhancements

1. **Minimum RBF Increment** - Require fee increase % for replacement
2. **Transaction Packages** - Replace multiple related transactions
3. **CPFP Support** - Child-pays-for-parent mechanisms
4. **Eviction by Age** - UTXO fragmentation management
5. **Ancestor/Descendant Limits** - Prevent chain bloat
6. **Replacement Cycles Detection** - Bounce attack prevention

## References

- BIP 125: Opt-in Full Replace-by-Fee Signaling
- Bitcoin Core: Transaction Pool Management
- Klomang Core: Transaction & UTXO Structure
