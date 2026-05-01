use klomang_core::MetricsCollector;
use lazy_static::lazy_static;
use prometheus::{
    register_counter, register_gauge, register_histogram, Counter, Encoder, Gauge, Histogram,
    TextEncoder,
};
use std::time::{Duration, Instant};

lazy_static! {
    pub static ref STORAGE_WRITE_LATENCY: Histogram = register_histogram!(
        "klomang_storage_write_latency_seconds",
        "Latency of WriteBatch commits"
    )
    .unwrap();
    pub static ref STORAGE_READ_LATENCY: Histogram = register_histogram!(
        "klomang_storage_read_latency_seconds",
        "Average latency of Get and MultiGet operations"
    )
    .unwrap();
    pub static ref STORAGE_COMPACTION_TIME: Histogram = register_histogram!(
        "klomang_storage_compaction_time_seconds",
        "Duration of background compaction processes"
    )
    .unwrap();
    pub static ref STORAGE_CACHE_HIT_RATIO: Gauge = register_gauge!(
        "klomang_storage_cache_hit_ratio",
        "Cache hit ratio for Block Cache (hits / (hits + misses))"
    )
    .unwrap();
    pub static ref STORAGE_BLOCKS_VALIDATED: Counter = register_counter!(
        "klomang_storage_blocks_validated_total",
        "Total number of blocks validated before storage"
    )
    .unwrap();
    pub static ref STORAGE_TRANSACTIONS_PROCESSED: Counter = register_counter!(
        "klomang_storage_transactions_processed_total",
        "Total number of transactions processed"
    )
    .unwrap();
}

/// Struct untuk mengelola metrics storage dengan integrasi Prometheus.
pub struct StorageMetrics {
    core_collector: Box<dyn MetricsCollector + Send + Sync>,
}

impl StorageMetrics {
    /// Buat instance baru dengan collector dari core.
    pub fn new(core_collector: Box<dyn MetricsCollector + Send + Sync>) -> Self {
        Self { core_collector }
    }

    /// Catat latency untuk operasi write.
    pub fn record_write_latency(&self, duration: Duration) {
        STORAGE_WRITE_LATENCY.observe(duration.as_secs_f64());
    }

    /// Catat latency untuk operasi read.
    pub fn record_read_latency(&self, duration: Duration) {
        STORAGE_READ_LATENCY.observe(duration.as_secs_f64());
    }

    /// Catat waktu compaction.
    pub fn record_compaction_time(&self, duration: Duration) {
        STORAGE_COMPACTION_TIME.observe(duration.as_secs_f64());
    }

    /// Update cache hit ratio.
    pub fn update_cache_hit_ratio(&self, ratio: f64) {
        STORAGE_CACHE_HIT_RATIO.set(ratio);
    }

    /// Catat blok yang divalidasi (delegasi ke core).
    pub fn record_block_validated(&self, count: u64) {
        STORAGE_BLOCKS_VALIDATED.inc_by(count as f64);
        self.core_collector.record_validated_blocks(count);
    }

    /// Catat transaksi yang diproses (delegasi ke core).
    pub fn record_transaction_processed(&self, count: u64) {
        STORAGE_TRANSACTIONS_PROCESSED.inc_by(count as f64);
        self.core_collector.record_processed_transactions(count);
    }

    /// Catat waktu validasi blok (delegasi ke core).
    pub fn record_block_validation_time(&self, duration: Duration) {
        self.core_collector.record_block_validation_time(duration);
    }

    /// Catat waktu pemrosesan transaksi (delegasi ke core).
    pub fn record_transaction_processing_time(&self, duration: Duration) {
        self.core_collector
            .record_transaction_processing_time(duration);
    }

    /// Ekspor metrics dalam format Prometheus.
    pub fn export_metrics() -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let encoder = TextEncoder::new();
        let metric_families = prometheus::gather();
        let mut buffer = Vec::new();
        encoder.encode(&metric_families, &mut buffer)?;
        Ok(String::from_utf8(buffer)?)
    }
}

/// Helper untuk mengukur waktu operasi secara otomatis.
pub struct LatencyTimer {
    start: Instant,
    metric_fn: Box<dyn Fn(Duration) + Send + Sync>,
}

impl LatencyTimer {
    pub fn new<F>(metric_fn: F) -> Self
    where
        F: Fn(Duration) + Send + Sync + 'static,
    {
        Self {
            start: Instant::now(),
            metric_fn: Box::new(metric_fn),
        }
    }
}

impl Drop for LatencyTimer {
    fn drop(&mut self) {
        let duration = self.start.elapsed();
        (self.metric_fn)(duration);
    }
}
