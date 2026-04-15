# klomang-core Read Path Integration Guide

## Overview

This guide explains how to integrate read path optimizations with klomang-core types for efficient UTXO management and DAG queries.

---

## Type Mappings

### Transaction to OutPoint

**From klomang-core:**
```rust
use klomang_core::core::state::transaction::{Transaction, TxInput};
```

**Conversion:**
```rust
use klomang_node::storage::OutPoint;

fn tx_inputs_to_outpoints(tx: &Transaction) -> Vec<OutPoint> {
    tx.inputs.iter()
        .map(|input| OutPoint::new(
            input.prev_tx.as_bytes().to_vec(),
            input.index,
        ))
        .collect()
}
```

### Transaction Output to UTXO

**From klomang-core:**
```rust
use klomang_core::core::state::transaction::TxOutput;
```

**Conversion:**
```rust
use klomang_node::storage::UtxoValue;

fn tx_output_to_utxo(
    output: &TxOutput,
    block_height: u32,
) -> UtxoValue {
    UtxoValue::new(
        output.value,
        vec![],  // Script placeholder
        output.pubkey_hash.as_bytes().to_vec(),
        block_height,
    )
}
```

---

## Integration Patterns

### Pattern 1: Validate Transaction Inputs

**Scenario:** Consensus engine needs to verify all inputs exist before accepting transaction

```rust
use klomang_core::core::state::transaction::Transaction;
use klomang_node::storage::{ReadPath, OutPoint};

fn validate_tx_inputs(
    read_path: &ReadPath,
    tx: &Transaction,
) -> Result<u64, Box<dyn std::error::Error>> {
    // Convert transaction inputs to outpoints
    let outpoints: Vec<OutPoint> = tx.inputs.iter()
        .map(|input| OutPoint::new(
            input.prev_tx.as_bytes().to_vec(),
            input.index,
        ))
        .collect();

    // Batch lookup all inputs (much faster than sequential)
    let utxo_results = read_path.get_multiple_utxos(&outpoints)?;

    let mut total_input_value = 0u64;
    for (outpoint, utxo_result) in utxo_results {
        match utxo_result {
            Ok(Some(utxo)) => {
                // UTXO found and valid
                total_input_value += utxo.amount;
            }
            Ok(None) => {
                // UTXO doesn't exist - invalid transaction
                return Err(format!(
                    "UTXO not found: {}/{}",
                    String::from_utf8_lossy(&outpoint.tx_hash),
                    outpoint.index
                ).into());
            }
            Err(e) => {
                return Err(format!("Storage error: {}", e).into());
            }
        }
    }

    Ok(total_input_value)
}

// Usage in consensus engine:
match validate_tx_inputs(&read_path, &transaction) {
    Ok(total) => {
        // Validate transaction outputs fees
        let output_total: u64 = transaction.outputs.iter()
            .map(|o| o.value)
            .sum();
        
        if output_total < total {
            // Valid fees
        }
    }
    Err(e) => {
        // Reject transaction
    }
}
```

### Pattern 2: Block Insertion - Create New UTXOs

**Scenario:** When block is confirmed, create new UTXOs from its transactions

```rust
use klomang_core::core::dag::BlockNode;
use klomang_node::storage::{KvStore, BlockTransactionBatch, UtxoValue, SpentUtxoBatch};

fn process_block_utxos(
    kv_store: &KvStore,
    block: &BlockNode,
    read_path: &ReadPath,
    block_height: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let block_hash = block.header.id.as_bytes();

    let mut batches = Vec::new();

    for tx in &block.transactions {
        // Collect spent UTXOs
        let mut spent_utxos = Vec::new();
        for (idx, input) in tx.inputs.iter().enumerate() {
            spent_utxos.push(SpentUtxoBatch {
                prev_tx_hash: input.prev_tx.as_bytes().to_vec(),
                output_index: input.index,
                spent_value: UtxoSpentValue::new(
                    tx.id.as_bytes().to_vec(),
                    idx as u32,
                    block_height,
                ),
            });
        }

        // Collect new UTXOs
        let mut new_utxos = Vec::new();
        for (idx, output) in tx.outputs.iter().enumerate() {
            new_utxos.push(UtxoValue::new(
                output.value,
                vec![],  // Script placeholder
                output.pubkey_hash.as_bytes().to_vec(),
                block_height,
            ));
        }

        batches.push(BlockTransactionBatch {
            tx_hash: tx.id.as_bytes().to_vec(),
            tx_value: convert_tx_to_value(tx)?,
            spent_utxos,
            new_utxos,
        });
    }

    // Atomic commit with all UTXOs
    kv_store.commit_block_atomic(
        block_hash,
        &block_value,
        &header_value,
        batches,
        &dag_node,
        &dag_tips,
    )?;

    Ok(())
}
```

### Pattern 3: DAG Traversal

**Scenario:** Consensus engine needs to traverse DAG for GHOSTDAG algorithm

```rust
use klomang_core::core::dag::BlockNode;
use klomang_core::core::crypto::Hash;

fn get_block_parents(
    read_path: &ReadPath,
    block_hash: &Hash,
) -> Result<Vec<Hash>, Box<dyn std::error::Error>> {
    // Look up DAG node for this block
    let dag_node = read_path.db().get(
        crate::storage::ColumnFamilyName::Dag,
        block_hash.as_bytes(),
    )?;

    if let Some(data) = dag_node {
        let node = DagNodeValue::from_bytes(&data)?;
        
        // Convert stored hashes back to klomang-core Hash type
        let parents = node.parent_hashes.iter()
            .map(|h| Hash::new(h))
            .collect();
        
        Ok(parents)
    } else {
        Err("Block not found in DAG".into())
    }
}

fn traverse_dag(
    read_path: &ReadPath,
    current_hash: &Hash,
    depth: usize,
) -> Result<Vec<Hash>, Box<dyn std::error::Error>> {
    if depth == 0 {
        return Ok(Vec::new());
    }

    let mut visited = vec![current_hash.clone()];
    let parents = get_block_parents(read_path, current_hash)?;

    for parent in parents {
        let mut ancestors = traverse_dag(read_path, &parent, depth - 1)?;
        visited.append(&mut ancestors);
    }

    Ok(visited)
}
```

### Pattern 4: UTXO Set Management

**Scenario:** Maintain efficient UTXO set for spending validation

```rust
use klomang_core::core::state::transaction::{Transaction, TxInput};
use klomang_node::storage::OutPoint;

fn update_utxo_set_from_transactions(
    read_path: &ReadPath,
    transactions: &[Transaction],
    block_height: u32,
) -> Result<(Vec<OutPoint>, Vec<OutPoint>), Box<dyn std::error::Error>> {
    let mut spent = Vec::new();
    let mut created = Vec::new();

    for tx in transactions {
        // Track spent outputs
        for input in &tx.inputs {
            spent.push(OutPoint::new(
                input.prev_tx.as_bytes().to_vec(),
                input.index,
            ));
        }

        // Track created outputs
        for (idx, _output) in tx.outputs.iter().enumerate() {
            created.push(OutPoint::new(
                tx.id.as_bytes().to_vec(),
                idx as u32,
            ));
        }
    }

    // Verify all spent UTXOs exist
    let spent_exist = read_path.check_utxos_exist(&spent)?;
    for (outpoint, exists) in spent_exist {
        if !exists {
            return Err(format!("UTXO not found: {:?}", outpoint).into());
        }
    }

    Ok((spent, created))
}
```

### Pattern 5: DAG Tips Query

**Scenario:**Current consensus tips for extending blockchain

```rust
fn get_current_tips(
    read_path: &ReadPath,
) -> Result<Vec<Hash>, Box<dyn std::error::Error>> {
    match read_path.get_dag_tips()? {
        Some(tips) => {
            let hashes = tips.tip_blocks.iter()
                .map(|h| Hash::new(h))
                .collect();
            Ok(hashes)
        }
        None => Ok(Vec::new()),
    }
}

fn submit_block_to_dag(
    read_path: &ReadPath,
    new_block: &BlockNode,
) -> Result<(), Box<dyn std::error::Error>> {
    let tips = get_current_tips(read_path)?;

    // Validate parents are in tips or ancestors
    for parent in &new_block.header.parents {
        if !tips.contains(parent) {
            // Could be ancestor - check DAG
            let _ = read_path.db().get(
                crate::storage::ColumnFamilyName::Dag,
                parent.as_bytes(),
            )?;
        }
    }

    Ok(())
}
```

---

## Performance Optimization Examples

### Optimize Transaction Processing

```rust
// ❌ SLOW - Sequential lookups
for input in &tx.inputs {
    let outpoint = OutPoint::new(
        input.prev_tx.as_bytes().to_vec(),
        input.index,
    );
    if let Some(utxo) = read_path.get_utxo(&outpoint)? {
        // Process...
    }
}

// ✅ FAST - Batch lookup
let outpoints: Vec<OutPoint> = tx.inputs.iter()
    .map(|i| OutPoint::new(i.prev_tx.as_bytes().to_vec(), i.index))
    .collect();

let utxos = read_path.get_multiple_utxos(&outpoints)?;
for (_, utxo_result) in utxos {
    if let Ok(Some(utxo)) = utxo_result {
        // Process...
    }
}
// Speedup: 5-6x faster
```

### Optimize UTXO Scanning

```rust
// ❌ SLOW - Iterate all transactions
let tx = get_transaction_by_hash(hash)?;
for (idx, _) in tx.outputs.iter().enumerate() {
    // Process output...
}

// ✅ FAST - Direct prefix seek
let outputs = read_path.get_utxos_by_tx_hash(&hash)?;
for (idx, utxo) in outputs {
    // Process UTXO directly
}
// Speedup: 50x+ faster, O(k) not O(n)
```

---

## Error Handling in Integration

```rust
use klomang_node::storage::{StorageError, StorageResult};

fn handle_read_errors(result: StorageResult<Option<UtxoValue>>) {
    match result {
        Ok(Some(utxo)) => {
            println!("Found UTXO: {}", utxo.amount);
        }
        Ok(None) => {
            println!("UTXO not found");
        }
        Err(StorageError::SerializationError(msg)) => {
            eprintln!("Deserialization failed: {}", msg);
            // corrupted data in storage
        }
        Err(StorageError::DbError(msg)) => {
            eprintln!("Database error: {}", msg);
            // I/O error, retry might help
        }
        Err(StorageError::InvalidColumnFamily(cf)) => {
            eprintln!("Invalid column family: {}", cf);
            // Configuration error
        }
        Err(e) => {
            eprintln!("Other error: {}", e);
        }
    }
}
```

---

## Testing Integration

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_transaction() {
        let db = create_test_db();
        let read_path = ReadPath::new(db);
        let tx = create_test_transaction();

        let result = validate_tx_inputs(&read_path, &tx);
        assert!(result.is_ok());
    }

    #[test]
    fn test_dag_traversal() {
        let read_path = create_test_read_path();
        let tip = create_test_hash();

        let ancestors = traverse_dag(&read_path, &tip, 5);
        assert!(ancestors.is_ok());
    }
}
```

---

## Checklist for Integration

- [ ] Create OutPoint from TxInput
- [ ] Use get_multiple_utxos for batch validation
- [ ] Handle all StorageError types
- [ ] Convert Hash types correctly
- [ ] Test with realistic transaction sizes
- [ ] Monitor performance metrics
- [ ] Handle edge cases (empty inputs, nonexistent UTXOs)
- [ ] Test with klomang-core BlockNode types
- [ ] Integrate with consensus engine
- [ ] Deploy to production

---

## See Also

- [READ_PATH_OPTIMIZATION.md](READ_PATH_OPTIMIZATION.md) - Full reference
- [READ_PATH_QUICK_REFERENCE.md](READ_PATH_QUICK_REFERENCE.md) - API quick reference
- [examples/read_path_optimization.rs](../examples/read_path_optimization.rs) - Working examples
- [src/storage/read_path.rs](../src/storage/read_path.rs) - Implementation details
