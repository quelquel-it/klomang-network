# Key Schema Design for Klomang Storage

This document describes the Key-Value storage schema design implemented in `klomang-node/storage/`.

## Overview

The Key Schema Design defines the structure and organization of data stored in RocksDB for the Klomang blockchain node. Each data type is stored in a dedicated Column Family with a specific key-value format for optimal retrieval and management.

## Column Families

The implementation uses 9 column families to organize blockchain data:

| Column Family | Purpose | Key Format |
|---|---|---|
| **blocks** | Full block data | `block_hash` |
| **headers** | Block headers (lightweight) | `block_hash` |
| **transactions** | Full transaction data | `tx_hash` |
| **utxo** | Unspent transaction outputs | `tx_hash + output_index` |
| **utxo_spent** | Spent UTXO index | `tx_hash + output_index` |
| **verkle_state** | Verkle tree state commitments | `commitment_path` |
| **dag** | DAG node structure | `block_hash` |
| **dag_tips** | Current DAG tip blocks | `tip_id` |
| **default** | System state and metadata | Variable |

## Key-Value Structures

### 1. Blocks (Column Family: `blocks`)

**Key:** `block_hash` (32 bytes)

**Value:**
```rust
BlockValue {
    hash: Vec<u8>,                              // Block hash
    header_bytes: Vec<u8>,                      // Serialized block header
    transactions: Vec<Vec<u8>>,                 // Transaction hashes in block
    timestamp: u64,                             // Block creation timestamp
}
```

**Serialization:** bincode

**Operations:**
- `put_block(hash, block)` - Store a new block
- `get_block(hash)` - Retrieve block by hash
- `delete_block(hash)` - Remove block

---

### 2. Headers (Column Family: `headers`)

**Key:** `block_hash` (32 bytes)

**Value:**
```rust
HeaderValue {
    block_hash: Vec<u8>,                        // Block hash
    parent_hashes: Vec<Vec<u8>>,                // Parent block hashes
    timestamp: u64,                             // Creation timestamp
    difficulty: u64,                            // Proof of Work difficulty
    nonce: u64,                                 // PoW nonce
    verkle_root: Vec<u8>,                       // Verkle state root
}
```

**Serialization:** bincode

**Operations:**
- `put_header(hash, header)` - Store block header
- `get_header(hash)` - Retrieve header
- `delete_header(hash)` - Remove header

---

### 3. Transactions (Column Family: `transactions`)

**Key:** `tx_hash` (32 bytes)

**Value:**
```rust
TransactionValue {
    tx_hash: Vec<u8>,                           // Transaction hash
    inputs: Vec<TransactionInput>,              // Transaction inputs
    outputs: Vec<TransactionOutput>,            // Transaction outputs
    fee: u64,                                   // Transaction fee
}

TransactionInput {
    previous_tx_hash: Vec<u8>,                  // Previous UTXO tx hash
    output_index: u32,                          // Output index to spend
}

TransactionOutput {
    amount: u64,                                // Output amount
    script: Vec<u8>,                            // Output script code
    owner: Vec<u8>,                             // Owner address
}
```

**Serialization:** bincode

**Operations:**
- `put_transaction(hash, tx)` - Store transaction
- `get_transaction(hash)` - Retrieve transaction
- `delete_transaction(hash)` - Remove transaction

---

### 4. UTXO (Column Family: `utxo`)

**Key:** `tx_hash (32 bytes) + output_index (4 bytes LE)` = 36 bytes

**Value:**
```rust
UtxoValue {
    amount: u64,                                // Output amount in satoshis
    script: Vec<u8>,                            // Script code
    owner: Vec<u8>,                             // Owner address
    block_height: u32,                          // Block where created
}
```

**Serialization:** bincode

**Key Helper Functions:**
- `make_utxo_key(tx_hash, output_index)` - Create composite key
- `parse_utxo_key(key)` - Parse composite key

**Operations:**
- `put_utxo(tx_hash, index, utxo)` - Store UTXO
- `get_utxo(tx_hash, index)` - Retrieve UTXO
- `delete_utxo(tx_hash, index)` - Remove UTXO
- `utxo_exists(tx_hash, index)` - Check UTXO existence

---

### 5. UTXO Spent Index (Column Family: `utxo_spent`)

**Key:** `tx_hash (32 bytes) + output_index (4 bytes LE)` = 36 bytes

**Value:**
```rust
UtxoSpentValue {
    spent_by_tx_hash: Vec<u8>,                  // Transaction that spent it
    input_index: u32,                           // Input index in spending tx
    spent_at_block_height: u32,                 // Block where spent
}
```

**Serialization:** bincode

**Operations:**
- `put_utxo_spent(tx_hash, index, spent)` - Mark UTXO as spent
- `get_utxo_spent(tx_hash, index)` - Get spending information
- `delete_utxo_spent(tx_hash, index)` - Remove spent record

---

### 6. Verkle State (Column Family: `verkle_state`)

**Key:** `commitment_path` (variable length)

**Value:**
```rust
VerkleStateValue {
    commitment_path: Vec<u8>,                   // Path in Verkle tree
    node_value: Vec<u8>,                        // Node commitment value
    is_leaf: bool,                              // Whether this is a leaf
}
```

**Serialization:** bincode

**Operations:**
- `put_verkle_state(path, state)` - Store Verkle node
- `get_verkle_state(path)` - Retrieve Verkle node
- `delete_verkle_state(path)` - Remove Verkle node

---

### 7. DAG Nodes (Column Family: `dag`)

**Key:** `block_hash` (32 bytes)

**Value:**
```rust
DagNodeValue {
    block_hash: Vec<u8>,                        // Block hash
    parent_hashes: Vec<Vec<u8>>,                // Parent block hashes
    blue_set: Vec<Vec<u8>>,                     // Blue set (GhostDAG)
    red_set: Vec<Vec<u8>>,                      // Red set (GhostDAG)
    blue_score: u64,                            // Blue score (ordering metric)
}
```

**Serialization:** bincode

**Operations:**
- `put_dag_node(hash, node)` - Store DAG node
- `get_dag_node(hash)` - Retrieve DAG node
- `delete_dag_node(hash)` - Remove DAG node

---

### 8. DAG Tips (Column Family: `dag_tips`)

**Key:** `tip_id` (variable, typically `"current_tips"`)

**Value:**
```rust
DagTipsValue {
    tip_blocks: Vec<Vec<u8>>,                   // Hashes of tip blocks
    last_updated_height: u32,                   // Last update height
}
```

**Serialization:** bincode

**Operations:**
- `put_dag_tips(key, tips)` - Store DAG tips
- `get_dag_tips(key)` - Retrieve DAG tips
- `delete_dag_tips(key)` - Remove DAG tips
- `put_current_dag_tips(tips)` - Store current tips at default key
- `get_current_dag_tips()` - Get current tips

---

## Usage Examples

### Store and Retrieve a Block

```rust
use klomang_node::storage::{StorageDb, StorageConfig, KvStore, BlockValue};

let config = StorageConfig::new("./blockchain_data");
let db = StorageDb::open_with_config(&config)?;
let kv_store = KvStore::new(db);

let block = BlockValue {
    hash: block_hash.to_vec(),
    header_bytes: header_data,
    transactions: tx_list,
    timestamp: current_time,
};

kv_store.put_block(&block_hash, &block)?;
if let Some(retrieved) = kv_store.get_block(&block_hash)? {
    println!("Block retrieved: {:?}", retrieved);
}
```

### UTXO Management

```rust
use klomang_node::storage::{KvStore, UtxoValue};

// Store a UTXO
let utxo = UtxoValue::new(5_000_000, script, owner, 100);
kv_store.put_utxo(&tx_hash, 0, &utxo)?;

// Check if UTXO exists and spend it
if kv_store.utxo_exists(&tx_hash, 0)? {
    let spent = UtxoSpentValue::new(spending_tx, 0, 101);
    kv_store.put_utxo_spent(&tx_hash, 0, &spent)?;
}
```

## Implementation Files

- `schema.rs` - Data structure definitions and serialization helpers
- `cf.rs` - Column family definitions
- `kv_store.rs` - Strongly-typed key-value store operations
- `db.rs` - Low-level RocksDB wrapper
- `error.rs` - Error types and handling

## Serialization

All values are serialized using **bincode** for efficient binary encoding:
- **Advantages:** Fast, compact, deterministic
- **Format:** Binary, not human-readable
- **Error Handling:** Returns `StorageError::SerializationError` on failure

## Thread Safety

- `KvStore` is thread-safe (uses `Arc<DB>` internally)
- Multiple threads can operate on the same database simultaneously
- All operations are atomic at the RocksDB level

## Performance Characteristics

| Operation | Complexity | Notes |
|---|---|---|
| Put | O(log n) | RocksDB LSM tree write |
| Get | O(log n) | Read from SST files |
| Delete | O(log n) | Marked as deleted |
| Range query | O(log n + k) | k = result count |
| Flush | O(n) | Writes to disk |
| Compact | O(n log n) | Background operation |

## Future Extensions

Potential enhancements for tighter klomang-core integration:
1. Use `klomang_core` types directly for serialization
2. Implement custom serde encoders for klomang-core structs
3. Add batch transaction support for multi-row operations
4. Implement snapshot-based backups
5. Add bloom filters for existence checks
