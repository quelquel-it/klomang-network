# klomang-core Integration Guide

## Overview

This guide shows how to integrate klomang-core types (BlockNode, Transaction, TxInput, TxOutput) with the atomic write path in klomang-node storage.

## Type Mapping

### BlockNode → Storage Types

```rust
use klomang_core::core::dag::{BlockNode, BlockHeader};
use klomang_node::storage::{BlockValue, HeaderValue, DagNodeValue};

// BlockNode from klomang-core
let block_node: BlockNode = /* from consensus engine */;

// Convert to storage types
let block_value = BlockValue {
    hash: block_node.header.id.as_bytes().to_vec(),
    header_bytes: bincode::serialize(&block_node.header)?,
    transactions: block_node.transactions.iter()
        .map(|tx| tx.id.as_bytes().to_vec())
        .collect(),
    timestamp: block_node.header.timestamp,
};

let header_value = HeaderValue {
    block_hash: block_node.header.id.as_bytes().to_vec(),
    parent_hashes: block_node.header.parents.iter()
        .map(|p| p.as_bytes().to_vec())
        .collect(),
    timestamp: block_node.header.timestamp,
    difficulty: block_node.header.difficulty,
    nonce: block_node.header.nonce,
    verkle_root: block_node.header.verkle_root.as_bytes().to_vec(),
};
```

### Transaction → BlockTransactionBatch

```rust
use klomang_core::core::state::transaction::{Transaction, TxInput, TxOutput};
use klomang_node::storage::{
    BlockTransactionBatch, SpentUtxoBatch, TransactionValue, TransactionInput,
    TransactionOutput, UtxoValue, UtxoSpentValue,
};

fn convert_transaction_to_batch(
    tx: &Transaction,
    block_height: u32,
    block_hash: &[u8],
    input_index_in_block: usize,
) -> StorageResult<BlockTransactionBatch> {
    // Convert transaction data
    let inputs: Vec<TransactionInput> = tx.inputs.iter()
        .map(|input| TransactionInput {
            previous_tx_hash: input.prev_tx.as_bytes().to_vec(),
            output_index: input.index,
        })
        .collect();

    let outputs: Vec<TransactionOutput> = tx.outputs.iter()
        .map(|output| TransactionOutput {
            amount: output.value,
            script: vec![],  // Empty for now, extend as needed
            owner: output.pubkey_hash.as_bytes().to_vec(),
        })
        .collect();

    let tx_value = TransactionValue {
        tx_hash: tx.id.as_bytes().to_vec(),
        inputs,
        outputs,
        fee: calculate_fee(tx),  // Application-specific
    };

    // Track spent UTXOs
    let mut spent_utxos = Vec::new();
    for (idx, input) in tx.inputs.iter().enumerate() {
        spent_utxos.push(SpentUtxoBatch {
            prev_tx_hash: input.prev_tx.as_bytes().to_vec(),
            output_index: input.index,
            spent_value: UtxoSpentValue::new(
                tx.id.as_bytes().to_vec(),
                idx as u32,  // input index in this transaction
                block_height,
            ),
        });
    }

    // Create new UTXOs
    let mut new_utxos = Vec::new();
    for (idx, output) in tx.outputs.iter().enumerate() {
        new_utxos.push(UtxoValue::new(
            output.value,
            vec![],  // Script placeholder - extend as needed
            output.pubkey_hash.as_bytes().to_vec(),
            block_height,
        ));
    }

    Ok(BlockTransactionBatch {
        tx_hash: tx.id.as_bytes().to_vec(),
        tx_value,
        spent_utxos,
        new_utxos,
    })
}
```

### DAG Conversion

```rust
use klomang_core::core::dag::BlockNode;
use klomang_node::storage::DagNodeValue;

fn convert_dag_node(
    block_node: &BlockNode,
    block_hash: &[u8],
) -> DagNodeValue {
    DagNodeValue::new(
        block_hash.to_vec(),
        block_node.header.parents.iter()
            .map(|p| p.as_bytes().to_vec())
            .collect(),
        block_node.blue_set.iter()
            .map(|b| b.as_bytes().to_vec())
            .collect(),
        block_node.red_set.iter()
            .map(|r| r.as_bytes().to_vec())
            .collect(),
        block_node.blue_score,
    )
}
```

## Complete Integration Example

```rust
use klomang_core::core::dag::BlockNode;
use klomang_node::storage::{KvStore, StorageDb, StorageConfig, DagTipsValue};

pub fn commit_block_from_core(
    kv_store: &KvStore,
    block_node: BlockNode,
    block_height: u32,
    current_tips: &[Vec<u8>],
) -> StorageResult<()> {
    let block_hash = block_node.header.id.as_bytes();

    // Step 1: Convert block and header
    let block_value = BlockValue {
        hash: block_hash.to_vec(),
        header_bytes: bincode::serialize(&block_node.header)
            .map_err(|e| StorageError::SerializationError(e.to_string()))?,
        transactions: block_node.transactions.iter()
            .map(|tx| tx.id.as_bytes().to_vec())
            .collect(),
        timestamp: block_node.header.timestamp,
    };

    let header_value = HeaderValue {
        block_hash: block_hash.to_vec(),
        parent_hashes: block_node.header.parents.iter()
            .map(|p| p.as_bytes().to_vec())
            .collect(),
        timestamp: block_node.header.timestamp,
        difficulty: block_node.header.difficulty,
        nonce: block_node.header.nonce,
        verkle_root: block_node.header.verkle_root.as_bytes().to_vec(),
    };

    // Step 2: Convert transactions
    let mut batches = Vec::new();
    for tx in &block_node.transactions {
        let batch = convert_transaction_to_batch(
            tx,
            block_height,
            block_hash,
            0,  // Adjust as needed
        )?;
        batches.push(batch);
    }

    // Step 3: Convert DAG node
    let dag_node = convert_dag_node(&block_node, block_hash);

    // Step 4: Update DAG tips
    let dag_tips = DagTipsValue::new(
        current_tips.to_vec(),
        block_height,
    );

    // Step 5: Atomic commit
    kv_store.commit_block_atomic(
        block_hash,
        &block_value,
        &header_value,
        batches,
        &dag_node,
        &dag_tips,
    )
}
```

## Type Correspondence Table

| klomang-core | klomang-node storage |
|---|---|
| `BlockNode` | `BlockValue` + `HeaderValue` + `DagNodeValue` |
| `BlockHeader` | `HeaderValue` |
| `Transaction` | `BlockTransactionBatch` + `TransactionValue` |
| `TxInput` | `SpentUtxoBatch` + `TransactionInput` |
| `TxOutput` | `TransactionOutput` + `UtxoValue` |
| `Hash` | `Vec<u8>` |

## Hash Conversion

```rust
use klomang_core::core::crypto::Hash;

// From klomang-core Hash
let hash: Hash = /* ... */;
let bytes = hash.as_bytes().to_vec();  // Convert to Vec<u8>

// Create Hash from bytes
let bytes: Vec<u8> = /* ... */;
let hash = Hash::new(&bytes);
```

## Field Mapping Reference

### BlockHeader → HeaderValue
```
BlockHeader::id → HeaderValue::block_hash
BlockHeader::parents → HeaderValue::parent_hashes (Vec)
BlockHeader::timestamp → HeaderValue::timestamp
BlockHeader::difficulty → HeaderValue::difficulty
BlockHeader::nonce → HeaderValue::nonce
BlockHeader::verkle_root → HeaderValue::verkle_root
```

### Transaction → TransactionValue
```
Transaction::id → TransactionValue::tx_hash
Transaction::inputs → TransactionValue::inputs (converted)
Transaction::outputs → TransactionValue::outputs (converted)
(fee calculated) → TransactionValue::fee
```

### TxInput → SpentUtxoBatch + TransactionInput
```
TxInput::prev_tx → SpentUtxoBatch::prev_tx_hash
TxInput::index → SpentUtxoBatch::output_index
(auto) → SpentUtxoBatch::spent_value (UtxoSpentValue)
```

### TxOutput → TransactionOutput + UtxoValue
```
TxOutput::value → TransactionOutput::amount
TxOutput::pubkey_hash → TransactionOutput::owner + UtxoValue::owner
(script) → TransactionOutput::script + UtxoValue::script
```

## Error Handling During Conversion

```rust
// Serialization can fail
let header_bytes = bincode::serialize(&block_header)
    .map_err(|e| StorageError::SerializationError(format!(
        "Failed to serialize header: {}",
        e
    )))?;

// Hash conversion generally safe
let hash_bytes = block_hash.as_bytes().to_vec();

// If any conversion fails, the atomic commit never happens
let batches = transactions.iter()
    .map(|tx| convert_transaction_to_batch(tx, height, hash, 0))
    .collect::<Result<Vec<_>, _>>()?;  // Early exit on error

// Now safe to commit
kv_store.commit_block_atomic(/*...*/)?;
```

## Performance Considerations

**Conversion Time (approximate per block):**
- Block header conversion: ~0.01ms
- Transaction conversions: ~0.1ms per transaction
- DAG conversion: ~0.05ms
- Total preparation: ~0.5ms for 100-tx block
- Atomic commit: ~1-2ms

## Testing Conversion Functions

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_conversion() {
        let block_node = create_test_block_node();
        let block_value = convert_block_value(&block_node)?;
        
        assert_eq!(block_value.hash, block_node.header.id.as_bytes());
        assert_eq!(block_value.timestamp, block_node.header.timestamp);
    }

    #[test]
    fn test_transaction_conversion() {
        let tx = create_test_transaction();
        let batch = convert_transaction_to_batch(&tx, 1, hash, 0)?;
        
        assert_eq!(batch.new_utxos.len(), tx.outputs.len());
        assert_eq!(batch.spent_utxos.len(), tx.inputs.len());
    }
}
```

## Integration Checklist

- [ ] Import klomang-core types in your module
- [ ] Implement block conversion function
- [ ] Implement transaction conversion function
- [ ] Implement DAG conversion function
- [ ] Handle Hash → Vec<u8> conversions
- [ ] Test conversion with sample data
- [ ] Integrate with consensus engine
- [ ] Handle errors during conversion
- [ ] Verify atomic commits succeed
- [ ] Monitor and log conversions in production

## See Also

- `ATOMIC_WRITE_PATH.md` - Full atomic write documentation
- `ATOMIC_WRITE_QUICK_REFERENCE.md` - Quick API reference
- `src/storage/schema.rs` - Storage type definitions
- `examples/atomic_block_commit.rs` - Usage examples
