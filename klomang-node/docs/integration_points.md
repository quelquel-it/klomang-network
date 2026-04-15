# Integration Points Documentation

## Overview

The integration points module provides functional bridges between klomang-node storage components and klomang-core validation logic. This enables seamless data flow between the node's persistent storage layer and the core blockchain validation engine.

## Components

### CoreIntegration

Provides direct access to storage operations for core validation:

- `get_utxo(outpoint: &CoreOutPoint) -> StorageResult<Option<UtxoValue>>`: Retrieves UTXO data for transaction validation
- `apply_transaction_state(tx: &Transaction, batch: &mut WriteBatch)`: Maps core transaction processing to database operations

### MempoolStorage

Handles persistent transaction storage across node restarts:

- `store_transaction(tx: &Transaction)`: Persists transactions to mempool
- `remove_transaction(tx_hash: &Hash)`: Removes transactions from persistent mempool
- `load_all_transactions() -> Vec<Transaction>`: Loads all transactions on startup

### NetworkStorage

Manages network-received data with core validation:

- `store_block_from_network(block: &BlockNode)`: Stores validated blocks from network
- `store_tx_from_network(tx: &Transaction)`: Stores validated transactions from network

## Type Alignment

The integration handles type differences between node and core:

- Core `OutPoint = (Hash, u32)` vs Node `OutPoint` struct
- Core `Transaction` vs Node `TransactionValue` serialization
- Core `BlockNode` vs Node `BlockValue` storage format

## Usage Example

```rust
use klomang_node::storage::{StorageEngine, CoreIntegration, MempoolStorage, NetworkStorage};

// Initialize storage engine
let storage = Arc::new(StorageEngine::new(db)?);

// Create integration components
let core_integration = Arc::new(CoreIntegration::new(Arc::clone(&storage)));
let mempool_storage = MempoolStorage::new(Arc::clone(&storage));
let network_storage = NetworkStorage::new(Arc::clone(&storage), Arc::clone(&core_integration));

// Use for transaction validation
if let Some(utxo) = core_integration.get_utxo(&outpoint)? {
    // Validate transaction inputs
}

// Store network transactions
mempool_storage.store_transaction(&tx)?;
```

## Error Handling

All integration functions return `StorageResult<T>` which wraps storage-specific errors. Core validation errors are handled separately in the core layer.

## Performance Considerations

- UTXO lookups use the hot cache layer for fast access
- Write operations are queued through the concurrency layer
- Batch operations minimize database round trips
- Parallel reads utilize the Rayon thread pool