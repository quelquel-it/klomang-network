use std::time::Duration;

/// Trait untuk pengumpulan metrik performa protokol secara global.
/// Implementasi ini memungkinkan pencatatan non-blocking untuk metrik seperti waktu validasi blok.
pub trait MetricsCollector {
    /// Catat waktu validasi blok sebelum ditulis ke storage.
    fn record_block_validation_time(&self, duration: Duration);

    /// Catat waktu pemrosesan transaksi dalam protokol.
    fn record_transaction_processing_time(&self, duration: Duration);

    /// Catat jumlah blok yang divalidasi.
    fn record_validated_blocks(&self, count: u64);

    /// Catat jumlah transaksi yang diproses.
    fn record_processed_transactions(&self, count: u64);
}

/// Implementasi default yang tidak melakukan apa-apa.
/// Digunakan ketika metrics tidak diperlukan atau dalam mode testing.
pub struct NoOpMetricsCollector;

impl MetricsCollector for NoOpMetricsCollector {
    fn record_block_validation_time(&self, _duration: Duration) {}

    fn record_transaction_processing_time(&self, _duration: Duration) {}

    fn record_validated_blocks(&self, _count: u64) {}

    fn record_processed_transactions(&self, _count: u64) {}
}