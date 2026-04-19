/// Background Batch Processor untuk Deferred Resolution
/// 
/// Modul ini menyediakan background task yang secara berkala memproses batch
/// dari deferred orphan resolutions menggunakan DeferredResolver. Ini mencegah
/// CPU spikes dan memastikan smooth transaction adoption.

use std::sync::Arc;
use std::time::Duration;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use super::advanced_orphan_management::DeferredResolver;

/// Statistik untuk background batch processor
#[derive(Debug, Clone)]
pub struct BackgroundProcessorStats {
    /// Total batches processed sejak start
    pub total_batches_processed: u64,
    /// Total tasks successfully processed
    pub total_tasks_processed: u64,
    /// Total expired tasks (TTL exceeded)
    pub total_expired_tasks: u64,
    /// Average items per batch
    pub avg_items_per_batch: f64,
    /// Last batch size
    pub last_batch_size: usize,
    /// Is processor currently running
    pub is_running: bool,
}

/// Background Batch Processor untuk DeferredResolver
pub struct BackgroundBatchProcessor {
    /// Reference ke deferred resolver
    deferred_resolver: Arc<DeferredResolver>,
    
    /// Processing interval dalam milliseconds
    processing_interval_ms: u64,
    
    /// Is processor running
    is_running: Arc<AtomicBool>,
    
    /// Statistics
    stats: Arc<Mutex<BackgroundProcessorStats>>,
}

impl BackgroundBatchProcessor {
    /// Create new background batch processor
    pub fn new(
        deferred_resolver: Arc<DeferredResolver>,
        processing_interval_ms: u64,
    ) -> Self {
        Self {
            deferred_resolver,
            processing_interval_ms,
            is_running: Arc::new(AtomicBool::new(false)),
            stats: Arc::new(Mutex::new(BackgroundProcessorStats {
                total_batches_processed: 0,
                total_tasks_processed: 0,
                total_expired_tasks: 0,
                avg_items_per_batch: 0.0,
                last_batch_size: 0,
                is_running: false,
            })),
        }
    }

    /// Start background processor (menjalankan dalam thread)
    pub fn start(&self) -> Result<(), String> {
        if self.is_running.load(Ordering::SeqCst) {
            return Err("Processor already running".to_string());
        }

        self.is_running.store(true, Ordering::SeqCst);
        
        let mut stats = self.stats.lock();
        stats.is_running = true;
        drop(stats);

        Ok(())
    }

    /// Stop background processor
    pub fn stop(&self) {
        self.is_running.store(false, Ordering::SeqCst);
        
        let mut stats = self.stats.lock();
        stats.is_running = false;
    }

    /// Process single batch dari deferred queue
    pub fn process_batch_once(&self) -> Result<usize, String> {
        if !self.is_running.load(Ordering::SeqCst) {
            return Err("Processor not running".to_string());
        }

        // Process batch dari deferred resolver
        let batch = self.deferred_resolver.process_batch()?;
        let batch_size = batch.len();

        if batch_size > 0 {
            let mut stats = self.stats.lock();
            stats.total_batches_processed += 1;
            stats.total_tasks_processed += batch_size as u64;
            stats.last_batch_size = batch_size;
            
            // Update average
            if stats.total_batches_processed > 0 {
                stats.avg_items_per_batch = stats.total_tasks_processed as f64 
                    / stats.total_batches_processed as f64;
            }
        }

        Ok(batch_size)
    }

    /// Get processor statistics
    pub fn get_stats(&self) -> BackgroundProcessorStats {
        self.stats.lock().clone()
    }

    /// Clear all statistics
    pub fn clear_stats(&self) {
        let mut stats = self.stats.lock();
        stats.total_batches_processed = 0;
        stats.total_tasks_processed = 0;
        stats.total_expired_tasks = 0;
        stats.avg_items_per_batch = 0.0;
        stats.last_batch_size = 0;
    }

    /// Get processing interval
    pub fn get_processing_interval(&self) -> Duration {
        Duration::from_millis(self.processing_interval_ms)
    }

    /// Get queue size (useful for monitoring)
    pub fn get_queue_size(&self) -> usize {
        self.deferred_resolver.queue_size()
    }

    /// Check if processor is running
    pub fn is_running(&self) -> bool {
        self.is_running.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_background_processor_creation() {
        let resolver = Arc::new(super::super::advanced_orphan_management::DeferredResolver::new(10, 1000));
        let processor = BackgroundBatchProcessor::new(resolver, 100);
        
        assert_eq!(processor.processing_interval_ms, 100);
        assert!(!processor.is_running());
    }

    #[test]
    fn test_processor_start_stop() {
        let resolver = Arc::new(super::super::advanced_orphan_management::DeferredResolver::new(10, 1000));
        let processor = BackgroundBatchProcessor::new(resolver, 100);
        
        assert!(processor.start().is_ok());
        assert!(processor.is_running());
        
        processor.stop();
        assert!(!processor.is_running());
    }

    #[test]
    fn test_processor_statistics() {
        let resolver = Arc::new(super::super::advanced_orphan_management::DeferredResolver::new(10, 1000));
        let processor = BackgroundBatchProcessor::new(resolver, 100);
        
        let stats = processor.get_stats();
        assert_eq!(stats.total_batches_processed, 0);
        assert_eq!(stats.total_tasks_processed, 0);
        
        processor.clear_stats();
        let stats = processor.get_stats();
        assert_eq!(stats.total_batches_processed, 0);
    }

    #[test]
    fn test_processor_interval() {
        let resolver = Arc::new(super::super::advanced_orphan_management::DeferredResolver::new(10, 1000));
        let processor = BackgroundBatchProcessor::new(resolver, 500);
        
        assert_eq!(processor.get_processing_interval(), Duration::from_millis(500));
    }
}
