# Atomic Write Path Quick Reference

## Quick Start

### Basic Block Commitment
```rust
use klomang_node::storage::{KvStore, StorageDb, StorageConfig};

// Setup
let db = StorageDb::open_with_config(&config)?;
let kv_store = KvStore::new(db);

// Commit block atomically
kv_store.commit_block_atomic(
    block_hash,
    &block_value,
    &header_value,
    transactions,  // Vec<BlockTransactionBatch>
    &dag_node,
    &dag_tips,
)?;
```

## Data Structure Quick Reference

### BlockTransactionBatch
```rust
pub struct BlockTransactionBatch {
    pub tx_hash: Vec<u8>,
    pub tx_value: TransactionValue,
    pub spent_utxos: Vec<SpentUtxoBatch>,  // UTXOs being consumed
    pub new_utxos: Vec<UtxoValue>,         // UTXOs being created
}
```

### SpentUtxoBatch
```rust
pub struct SpentUtxoBatch {
    pub prev_tx_hash: Vec<u8>,             // Which TX created this UTXO
    pub output_index: u32,                 // Output index
    pub spent_value: UtxoSpentValue,       // Spending info
}
```

## API Reference

### KvStore Methods

```rust
// Atomic commit with durability (WAL enabled)
pub fn commit_block_atomic(
    &self,
    block_hash: &[u8],
    block_value: &BlockValue,
    header_value: &HeaderValue,
    transactions: Vec<BlockTransactionBatch>,
    dag_node: &DagNodeValue,
    dag_tips: &DagTipsValue,
) -> StorageResult<()>

// Atomic commit without WAL (faster, non-durable)
pub fn commit_block_atomic_no_wal(
    &self,
    block_hash: &[u8],
    block_value: &BlockValue,
    header_value: &HeaderValue,
    transactions: Vec<BlockTransactionBatch>,
    dag_node: &DagNodeValue,
    dag_tips: &DagTipsValue,
) -> StorageResult<()>
```

### AtomicBlockWriter Methods

```rust
// Direct access (lower-level)
pub fn commit_block_to_storage(
    db: &StorageDb,
    block_hash: &[u8],
    block_value: &BlockValue,
    header_value: &HeaderValue,
    transactions: Vec<BlockTransactionBatch>,
    dag_node: &DagNodeValue,
    dag_tips: &DagTipsValue,
) -> StorageResult<()>
```

### WriteBatch Type-Safe Methods

```rust
// Type-safe column family operations
batch.put_cf_typed(ColumnFamilyName::Blocks, key, value);
batch.delete_cf_typed(ColumnFamilyName::UtxoSpent, key);
```

## Error Handling

```rust
// All operations return Result
match kv_store.commit_block_atomic(...) {
    Ok(_) => println!("Block committed successfully"),
    Err(StorageError::SerializationError(msg)) => {
        // Data couldn't be serialized
        eprintln!("Serialization failed: {}", msg);
    }
    Err(StorageError::DbError(msg)) => {
        // Database error - serious issue
        eprintln!("Database error: {}", msg);
    }
    Err(e) => {
        // Other storage errors
        eprintln!("Error: {}", e);
    }
}
```

## Atomicity Guarantees

✅ **All-or-Nothing**
- Either entire block is committed or none of it
- No partial writes to database
- Consistent UTXO state
- Valid DAG structure

## Performance Characteristics

| Operation | Time |
|-----------|------|
| Batch preparation | ~0.1ms / transaction |
| Atomic commit | ~1ms / block |
| Serialization validation | Parallel possible |

## UTXO Key Format

```
[tx_hash (32 bytes) | output_index (4 bytes LE)]
= 36 bytes total
```

Use helper functions:
```rust
let key = make_utxo_key(tx_hash, output_index);
if let Some((tx, idx)) = parse_utxo_key(&key) {
    // Extract original components
}
```

## Column Families

| CF Name | Purpose | Key Format |
|---------|---------|-----------|
| blocks | Block data | block_hash |
| headers | Block headers | block_hash |
| transactions | Transaction data | tx_hash |
| utxo | Unspent outputs | tx_hash + output_index |
| utxo_spent | Spent tracking | tx_hash + output_index |
| verkle_state | Verkle tree | path |
| dag | DAG structure | block_hash |
| dag_tips | Current tips | "current_tips" |

## Common Patterns

### Pattern 1: Single Transaction Commit
```rust
let tx_batch = BlockTransactionBatch {
    tx_hash: tx.id.to_vec(),
    tx_value: tx_storage_value,
    spent_utxos: vec![/* ... */],
    new_utxos: vec![/* ... */],
};

kv_store.commit_block_atomic(
    block_hash,
    &block_value,
    &header_value,
    vec![tx_batch],  // Single transaction
    &dag_node,
    &dag_tips,
)?;
```

### Pattern 2: Complex Block
```rust
let mut transactions = Vec::new();

for tx in block.transactions {
    let mut spent = Vec::new();
    for input in &tx.inputs {
        spent.push(SpentUtxoBatch {
            prev_tx_hash: input.prev_tx.clone(),
            output_index: input.index,
            spent_value: UtxoSpentValue::new(
                tx.id.clone(),
                /* input_index */,
                block_height,
            ),
        });
    }

    let mut new = Vec::new();
    for (idx, output) in tx.outputs.iter().enumerate() {
        new.push(UtxoValue::new(
            output.value,
            output.script.clone(),
            output.owner.clone(),
            block_height,
        ));
    }

    transactions.push(BlockTransactionBatch {
        tx_hash: tx.id.to_vec(),
        tx_value: tx_as_storage_value,
        spent_utxos: spent,
        new_utxos: new,
    });
}

kv_store.commit_block_atomic(
    block_hash,
    &block_value,
    &header_value,
    transactions,
    &dag_node,
    &dag_tips,
)?;
```

### Pattern 3: Error Handling
```rust
// Prepare data with validation
if let Err(e) = prepare_block_data(&block) {
    eprintln!("Block data validation failed: {}", e);
    return;  // No writes to database
}

// If we get here, data is valid
kv_store.commit_block_atomic(/*...*/)?;
```

## When to Use

| Scenario | Use Atomic |
|----------|-----------|
| Regular block commit | ✅ Yes |
| Bulk sync snapshots | ⚠️ With no_wal |
| State repairs | ✅ Yes |
| Testing | ✅ Yes |
| Debug/logging only | ❌ No |

## Troubleshooting

| Error | Cause | Fix |
|-------|-------|-----|
| SerializationError | Data encoding failed | Check data types match schema |
| DbError | RocksDB I/O issue | Check disk space, permissions |
| InvalidColumnFamily | Wrong CF name | Use typed methods |

## See Also

- `ATOMIC_WRITE_PATH.md` - Full design documentation
- `examples/atomic_block_commit.rs` - Complete examples
- `src/storage/atomic_write.rs` - Implementation
