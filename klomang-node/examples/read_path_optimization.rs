// Example: Read Path Optimization with prefix seeks and multi-get
#![allow(dead_code)]

use std::sync::Arc;

use klomang_node::storage::{
    ReadPath, OutPoint, StorageDb, StorageConfig, KvStore, StorageCacheLayer,
};
use klomang_node::storage::metrics::StorageMetrics;
use klomang_core::core::metrics::NoOpMetricsCollector;

/// Example: Single UTXO lookup (standard)
pub fn example_single_utxo_lookup() -> Result<(), Box<dyn std::error::Error>> {
    let config = StorageConfig::new("./read_path_data");
    let db = StorageDb::open_with_config(&config, Arc::new(StorageMetrics::new(Box::new(NoOpMetricsCollector))))?;
    let cache_layer = StorageCacheLayer::new(db);
    let read_path = ReadPath::new(Arc::new(cache_layer));

    // Create an outpoint
    let tx_hash = vec![1u8; 32];
    let outpoint = OutPoint::new(tx_hash.clone(), 0);

    // Look up single UTXO
    match read_path.get_utxo(&outpoint)? {
        Some(utxo) => println!("✓ Found UTXO: amount={}", utxo.amount),
        None => println!("✗ UTXO not found"),
    }

    Ok(())
}

/// Example: Batch UTXO lookup (multi-get) - significantly faster
pub fn example_batch_utxo_lookup() -> Result<(), Box<dyn std::error::Error>> {
    let config = StorageConfig::new("./read_path_data");
    let db = StorageDb::open_with_config(&config, Arc::new(StorageMetrics::new(Box::new(NoOpMetricsCollector))))?;
    let cache_layer = StorageCacheLayer::new(db);
    let read_path = ReadPath::new(Arc::new(cache_layer));

    // Create multiple outpoints
    let outpoints = vec![
        OutPoint::new(vec![1u8; 32], 0),
        OutPoint::new(vec![1u8; 32], 1),
        OutPoint::new(vec![2u8; 32], 0),
        OutPoint::new(vec![3u8; 32], 0),
    ];

    // Batch lookup - single lock, overlapped I/O
    let results = read_path.get_multiple_utxos(&outpoints)?;

    println!("Batch lookup results:");
    for (_outpoint, result) in results {
        match result {
            Ok(Some(utxo)) => println!("  ✓ Found: amount={}", utxo.amount),
            Ok(None) => println!("  - Not found"),
            Err(e) => println!("  ✗ Error: {}", e),
        }
    }

    Ok(())
}

/// Example: Prefix seek - scan all outputs of a transaction
pub fn example_prefix_scan_by_tx_hash() -> Result<(), Box<dyn std::error::Error>> {
    let config = StorageConfig::new("./read_path_data");
    let db = StorageDb::open_with_config(&config, Arc::new(StorageMetrics::new(Box::new(NoOpMetricsCollector))))?;
    let cache_layer = StorageCacheLayer::new(db);
    let read_path = ReadPath::new(Arc::new(cache_layer));

    // Scan all outputs from specific transaction
    let tx_hash = vec![1u8; 32];
    let outputs = read_path.get_utxos_by_tx_hash(&tx_hash)?;

    println!("Transaction {} has {} outputs:", String::from_utf8_lossy(&tx_hash), outputs.len());
    for (output_index, utxo) in outputs {
        println!("  [{}] amount={}", output_index, utxo.amount);
    }

    Ok(())
}

/// Example: Range scan with upper bounds (memory efficient)
pub fn example_range_scan_with_bounds() -> Result<(), Box<dyn std::error::Error>> {
    let config = StorageConfig::new("./read_path_data");
    let db = StorageDb::open_with_config(&config, Arc::new(StorageMetrics::new(Box::new(NoOpMetricsCollector))))?;
    let cache_layer = StorageCacheLayer::new(db);
    let read_path = ReadPath::new(Arc::new(cache_layer));

    // Scan range with limit to prevent memory exhaustion
    let start_key = vec![1u8; 36]; // 32-byte hash + 4-byte index
    let end_key = vec![2u8; 36];
    let max_results = 1000;

    let results = read_path.scan_utxo_range(&start_key, &end_key, max_results)?;

    println!("Range scan returned {} results (max: {})", results.len(), max_results);
    for (key, utxo) in results.iter().take(5) {
        println!("  Key: {:?}, Amount: {}", &key[..8], utxo.amount);
    }

    Ok(())
}

/// Example: Get DAG tips efficiently
pub fn example_get_dag_tips() -> Result<(), Box<dyn std::error::Error>> {
    let config = StorageConfig::new("./read_path_data");
    let db = StorageDb::open_with_config(&config, Arc::new(StorageMetrics::new(Box::new(NoOpMetricsCollector))))?;
    let cache_layer = StorageCacheLayer::new(db);
    let read_path = ReadPath::new(Arc::new(cache_layer));

    match read_path.get_dag_tips()? {
        Some(tips) => {
            println!("✓ DAG Tips:");
            println!("  Block count: {}", tips.tip_blocks.len());
            println!("  Last updated: height {}", tips.last_updated_height);
        }
        None => println!("✗ No DAG tips found"),
    }

    Ok(())
}

/// Example: Scan DAG nodes
pub fn example_scan_dag_nodes() -> Result<(), Box<dyn std::error::Error>> {
    let config = StorageConfig::new("./read_path_data");
    let db = StorageDb::open_with_config(&config, Arc::new(StorageMetrics::new(Box::new(NoOpMetricsCollector))))?;
    let cache_layer = StorageCacheLayer::new(db);
    let read_path = ReadPath::new(Arc::new(cache_layer));

    let nodes = read_path.scan_dag_nodes(None, 10)?;

    println!("✓ Scanned {} DAG nodes", nodes.len());
    for (i, (_, node)) in nodes.iter().enumerate() {
        println!("  [{}] Blue score: {}", i, node.blue_score);
    }

    Ok(())
}

/// Example: Bulk existence check
pub fn example_bulk_existence_check() -> Result<(), Box<dyn std::error::Error>> {
    let config = StorageConfig::new("./read_path_data");
    let db = StorageDb::open_with_config(&config, Arc::new(StorageMetrics::new(Box::new(NoOpMetricsCollector))))?;
    let cache_layer = StorageCacheLayer::new(db);
    let read_path = ReadPath::new(Arc::new(cache_layer));

    let outpoints = vec![
        OutPoint::new(vec![1u8; 32], 0),
        OutPoint::new(vec![2u8; 32], 0),
        OutPoint::new(vec![3u8; 32], 0),
    ];

    let existence_map = read_path.check_utxos_exist(&outpoints)?;

    println!("Existence check results:");
    for (outpoint, exists) in existence_map {
        println!("  {:?}: {}", &outpoint.tx_hash[..8], if exists { "exists" } else { "not found" });
    }

    Ok(())
}

/// Example: Performance comparison - sequential vs batch
pub fn example_performance_comparison() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Instant;

    let config = StorageConfig::new("./read_path_perf_data");
    let db = StorageDb::open_with_config(&config, Arc::new(StorageMetrics::new(Box::new(NoOpMetricsCollector))))?;
    let cache_layer = Arc::new(StorageCacheLayer::new(db));
    let read_path = ReadPath::new(cache_layer.clone());
    let _kv_store = KvStore::new(cache_layer);

    // Prepare test outpoints
    let outpoints: Vec<OutPoint> = (0..100)
        .map(|i| OutPoint::new(vec![i as u8; 32], i as u32 % 10))
        .collect();

    println!("\nPerformance Comparison (100 UTXOs):\n");

    // Sequential lookups (slower)
    let start = Instant::now();
    let mut _sequential_count = 0;
    for outpoint in &outpoints {
        if let Ok(_) = read_path.get_utxo(outpoint) {
            _sequential_count += 1;
        }
    }
    let sequential_time = start.elapsed();
    println!("Sequential lookups: {:?}", sequential_time);

    // Batch lookups (faster)
    let start = Instant::now();
    let batch_results = read_path.get_multiple_utxos(&outpoints)?;
    let batch_time = start.elapsed();
    let _batch_count = batch_results.len();
    println!("Batch lookups (multi_get): {:?}", batch_time);

    // Calculate speedup
    if sequential_time > batch_time {
        let speedup = sequential_time.as_micros() as f64 / batch_time.as_micros() as f64;
        println!("\nSpeedup: {:.1}x faster\n", speedup);
    }

    Ok(())
}

/// Example: Prefix seek vs full scan (efficiency)
pub fn example_prefix_seek_efficiency() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Instant;

    let config = StorageConfig::new("./read_path_prefix_data");
    let db = StorageDb::open_with_config(&config, Arc::new(StorageMetrics::new(Box::new(NoOpMetricsCollector))))?;
    let cache_layer = Arc::new(StorageCacheLayer::new(db));
    let read_path = ReadPath::new(cache_layer);

    // Get all outputs from transaction (uses prefix seek)
    let tx_hash = vec![42u8; 32];

    let start = Instant::now();
    let outputs = read_path.get_utxos_by_tx_hash(&tx_hash)?;
    let elapsed = start.elapsed();

    println!("Prefix seek results:");
    println!("  Outputs found: {}", outputs.len());
    println!("  Time: {:?}", elapsed);
    println!("  Speed: O(k) where k = result count (not O(n) full scan)");

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== RocksDB Read Path Optimization Examples ===\n");

    println!("[1] Single UTXO Lookup");
    example_single_utxo_lookup().ok();

    println!("\n[2] Batch UTXO Lookup (Multi-Get)");
    example_batch_utxo_lookup().ok();

    println!("\n[3] Prefix Scan by Transaction Hash");
    example_prefix_scan_by_tx_hash().ok();

    println!("\n[4] Range Scan with Bounds");
    example_range_scan_with_bounds().ok();

    println!("\n[5] Get DAG Tips");
    example_get_dag_tips().ok();

    println!("\n[6] Scan DAG Nodes");
    example_scan_dag_nodes().ok();

    println!("\n[7] Bulk Existence Check");
    example_bulk_existence_check().ok();

    println!("\n[8] Performance Comparison");
    example_performance_comparison().ok();

    println!("\n[9] Prefix Seek Efficiency");
    example_prefix_seek_efficiency().ok();

    println!("\n=== All examples completed ===");
    Ok(())
}
