# Transaction Pool Core - Implementation Guide

## Overview
Transaction Pool Core (`klomang-node/src/mempool`) implementasi state machine untuk mengelola siklus hidup transaksi sebelum dibundel ke dalam blok.

## Modul Komponen

### 1. `status.rs` - Transaction Lifecycle State Machine
Mendefinisikan enum `TransactionStatus` dengan validasi transisi state:
- **Pending**: Transaksi diterima, belum divalidasi
- **Validated**: Input terverifikasi terhadap UTXO storage
- **InOrphanPool**: Input references belum ada, menunggu dependencies
- **InBlock**: Transaksi sudah included di blok
- **Rejected**: Transaksi gagal validasi, ditolak permanen

**State Transitions yang Valid:**
```
Pending ──→ Validated ──→ InBlock (terminal)
   ↓    ↘      ↓
   │     → Rejected (terminal)
   └──→ InOrphanPool ──→ Validated
```

**Features:**
- Safe transitions dengan error handling
- Status terminal check (InBlock, Rejected)
- TTL expiry support untuk orphan dan rejected

### 2. `pool.rs` - Multi-Indexed Transaction Storage
Struct `TransactionPool` dengan:
- **Primary Index**: By transaction hash (Vec<u8>) untuk lookup cepat
- **Secondary Index**: Orphan tracking list
- **Thread-Safe**: Menggunakan `parking_lot::RwLock`
- **Searchable**: By status, orphan queries

**PoolEntry Fields:**
```rust
pub struct PoolEntry {
    pub transaction: Transaction,       // Dari klomang-core
    pub status: TransactionStatus,
    pub arrival_time: u64,             // UNIX timestamp
    pub size_bytes: usize,             // Untuk fee rate calculation
    pub total_fee: u64,                // Dalam satoshis
}
```

**Key Methods:**
- `add_transaction()` - Tambah tx dengan fee validation
- `set_status()` - Transisi status dengan validasi
- `get_by_status()` - Query by status
- `get_orphans()` - Get orphan pool
- `cleanup_expired()` - Hapus expired entries
- `get_stats()` - Pool statistics

**Pool Configuration:**
```rust
pub struct PoolConfig {
    pub max_pool_size: usize,          // Default: 10000
    pub max_orphan_size: usize,        // Default: 1000
    pub min_fee_rate: u64,             // Default: 1 sat/byte
    pub orphan_ttl_secs: u64,          // Default: 600s
    pub rejected_ttl_secs: u64,        // Default: 3600s
}
```

### 3. `validation.rs` - UTXO Verification
Struct `PoolValidator` untuk:
- Validasi input transaksi terhadap UTXO storage
- Deteksi missing inputs (untuk orphan pool)
- Deterministic hasil across nodes

**Validation Results:**
- `Valid` - Semua inputs tersedia
- `MissingInputs(indices)` - Input tertentu belum ada
- `DoubleSpent` - Double-spend detected
- `InputNotFound(index)` - Specific input not found

**Integration dengan Storage:**
```rust
pub struct PoolValidator {
    kv_store: Arc<KvStore>,  // Akses UTXO storage
}
```

### 4. `selection.rs` - Deterministic Transaction Selection
Struct `DeterministicSelector` untuk block building dengan:
- **High Fee Strategy**: Prioritas fee rate tertinggi
- **FIFO Strategy**: Arrival time based
- **Deterministic Tie-Breaking**: Via transaction hash

**Selection Criteria:**
```rust
pub enum SelectionCriteria {
    MaxCount(usize),        // Jumlah transaksi max
    MaxBytes(usize),        // Size max
    MaxFees(u64),           // Total fee max
    Combined {...},         // Kombinasi ketiga
}
```

**Ordering Deterministik:**
1. Fee rate (tertinggi dulu)
2. Arrival time (tercepat dulu)
3. Transaction hash (lexicographic)

Menjamin semua node memilih transaksi yang sama untuk blok.

## Integration Architecture

```
TransactionPool (Data structure)
    ↓
PoolValidator (UTXO verification)
    ↓ (uses KvStore)
crate::storage::kv_store::KvStore
    ↓
crate::storage::cache::StorageCacheLayer
    ↓
RocksDB Storage
```

## Usage Example

```rust
use klomang_node::mempool::*;
use std::sync::Arc;

// Create pool
let pool = TransactionPool::default();

// Add transaction
pool.add_transaction(tx, total_fee, size_bytes)?;

// Validate against UTXO
let validator = PoolValidator::new(kv_store);
match validator.validate_transaction(&tx)? {
    ValidationResult::Valid => {
        pool.set_status(&tx_hash, TransactionStatus::Validated)?;
    }
    ValidationResult::MissingInputs(_) => {
        pool.set_status(&tx_hash, TransactionStatus::InOrphanPool)?;
    }
    _ => {}
}

// Select for block
let selector = DeterministicSelector::new(SelectionStrategy::HighestFee);
let block_txs = selector.select(
    pool.get_validated(),
    SelectionCriteria::Combined {
        max_count: 2000,
        max_bytes: 1_000_000,
        max_fees: 50_000_000,
    }
);
```

## Thread Safety

- **RwLock**: Semua pool access thread-safe
- **Arc**: Shared ownership across threads
- **No blocking**: Read operations tidak block writes

## Testing

Setiap modul memiliki unit tests untuk:
- State transitions
- Pool operations
- Validation logic
- Selection determinism

Run tests:
```bash
cargo test --lib mempool
```

## Key Design Decisions

1. **Multi-Index**: Memungkinkan lookup O(1) dan statistik cepat
2. **State Machine**: Explicit state transitions prevent invalid states
3. **Determinism**: Hash-based tie-breaking guarantee node consistency
4. **Expiry**: TTL untuk orphan/rejected prevent memory leak
5. **No Mocks**: Real UTXO verification untuk production use

## Performance Notes

- Pool lookup: O(1) by hash
- Add transaction: O(1) amortized
- Status query: O(n) with filter, consider caching for large pools
- Selection: O(n log n) for sorting
- Cleanup: O(n) with filter

## Future Enhancements

1. CPFPing (Child Pays For Parent) support
2. RBF (Replace-by-Fee) mechanism
3. Mempool synchronization between nodes
4. Eviction policies (when max_size reached)
5. Package-aware selection (for related transactions)
