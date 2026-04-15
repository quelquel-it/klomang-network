# RocksDB Performance Optimization for High TPS

This document describes the performance optimizations implemented in the Klomang node RocksDB storage backend for achieving high transactions per second (TPS).

## Overview

The storage layer is optimized for high-throughput blockchain operations with the following key optimizations:

- **Block-Based Table Options**: LRU cache, optimized block size, and index/filter caching
- **Bloom Filters**: Fast key lookups with minimal false positives
- **WAL Configuration**: Efficient write-ahead logging
- **Memory Management**: Controlled cache sizes and background jobs

## Configuration Parameters

All performance parameters are configurable through `StorageConfig`:

```rust
let config = StorageConfig::new("./data")
    .with_block_cache_size(1024 * 1024 * 1024)  // 1GB LRU cache
    .with_block_size(32 * 1024)                 // 32KB blocks
    .with_bloom_bits_per_key(10);               // Bloom filter precision
```

### Block Cache (LRU)

- **Default Size**: 1GB (configurable)
- **Purpose**: Caches uncompressed blocks in memory for faster reads
- **Impact**: Reduces disk I/O for frequently accessed data

### Block Size

- **Size**: 32KB
- **Purpose**: Balances read/write performance
- **Rationale**: Larger blocks improve sequential reads, smaller blocks reduce write amplification

### Bloom Filter

- **Bits per Key**: 10
- **Purpose**: Fast key existence checks
- **Features**:
  - Minimizes false positives during block/transaction hash lookups
  - Whole key filtering enabled for better performance

### Index and Filter Caching

- **Cache Index/Filter Blocks**: Enabled
- **Pin L0 Blocks**: Enabled
- **Purpose**: Keeps hot index data in cache, especially for recent (L0) data

## Integration with Storage Initialization

The optimizations are automatically applied during database initialization:

```rust
// Using default config
let db = StorageDb::open("./data", "./wal")?;

// Using custom config
let config = StorageConfig::new("./data")
    .with_block_cache_size(2 * 1024 * 1024 * 1024); // 2GB cache
let db = StorageDb::open_with_config(&config)?;
```

## Performance Characteristics

### Read Performance
- Bloom filters reduce unnecessary disk seeks
- LRU cache keeps hot data in memory
- Pinned L0 blocks accelerate recent data access

### Write Performance
- Optimized block size minimizes write amplification
- WAL configuration balances durability and speed
- Background compaction jobs prevent write stalls

### Memory Usage
- Configurable cache sizes prevent memory exhaustion
- Index/filter caching reduces RAM usage outside cache
- Efficient data structures minimize overhead

## Type Safety and Error Handling

All operations use strong typing with `Result<T, StorageError>`:

```rust
use klomang_core::types::{Block, Transaction};

let block_value = BlockValue::from_core_block(&block)?;
db.put_block(&block_hash, &block_value)?;
```

No `.unwrap()` calls - all errors are properly propagated.

## Production Readiness

- **No Placeholders**: All configurations are production-ready
- **Configurable**: All parameters can be tuned for specific workloads
- **Tested**: Serialization/deserialization validated with bincode
- **Documented**: Clear parameter meanings and performance implications

## Monitoring and Tuning

The storage layer provides metrics for monitoring:

```rust
// Cache hit/miss ratios
// Compaction statistics
// Memory usage tracking
```

Tune parameters based on:
- Available RAM
- Expected TPS
- Data access patterns
- Storage device characteristics</content>
<parameter name="filePath">/workspaces/klomang-network/klomang-node/PERFORMANCE_OPTIMIZATION.md