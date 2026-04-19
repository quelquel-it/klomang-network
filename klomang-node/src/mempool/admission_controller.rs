//! Advanced Admission Controller for Resource-Based Transaction Filtering
//!
//! This module implements real-time monitoring of system resources (CPU, RAM)
//! to dynamically adjust transaction admission policies based on current load.

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use parking_lot::RwLock;
use std::sync::Arc;

/// System resource metrics
#[derive(Clone, Debug)]
pub struct SystemMetrics {
    /// CPU utilization percentage (0-100)
    pub cpu_percent: f64,
    /// Available RAM percentage (0-100)
    pub ram_available_percent: f64,
    /// Last update timestamp
    pub timestamp: u64,
}

/// Admission control modes
#[derive(Clone, Debug, PartialEq)]
pub enum AdmissionMode {
    /// Normal admission - standard fee requirements
    Normal,
    /// Strict admission - 2x fee requirements when resources are critical
    Strict,
}

/// Advanced admission controller for resource-aware transaction filtering
pub struct AdmissionController {
    /// Current system metrics
    metrics: RwLock<SystemMetrics>,
    /// Current admission mode
    mode: RwLock<AdmissionMode>,
    /// CPU threshold for strict mode (%)
    cpu_threshold: f64,
    /// RAM available threshold for strict mode (%)
    ram_threshold: f64,
    /// How often to update metrics (seconds)
    update_interval_secs: u64,
    /// Last metrics update timestamp
    last_update: RwLock<u64>,
    /// KvStore for historical trend persistence
    kv_store: Option<std::sync::Arc<crate::storage::KvStore>>,
}

impl AdmissionController {
    /// Create new admission controller with default thresholds
    pub fn new() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            metrics: RwLock::new(SystemMetrics {
                cpu_percent: 0.0,
                ram_available_percent: 100.0,
                timestamp: now,
            }),
            mode: RwLock::new(AdmissionMode::Normal),
            cpu_threshold: 90.0, // 90% CPU usage
            ram_threshold: 10.0, // 10% RAM available
            update_interval_secs: 5, // Update every 5 seconds
            last_update: RwLock::new(now),
            kv_store: None,
        }
    }

    /// Create admission controller with KvStore for trend persistence
    pub fn with_kv_store(kv_store: Arc<crate::storage::KvStore>) -> Self {
        let mut controller = Self::new();
        controller.kv_store = Some(kv_store);
        controller
    }

    /// Update system metrics and admission mode
    pub fn update_metrics(&self) -> Result<(), String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Throttle updates to avoid excessive I/O
        if now.saturating_sub(*self.last_update.read()) < self.update_interval_secs {
            return Ok(());
        }

        // Read CPU usage
        let cpu_percent = self.read_cpu_usage()?;

        // Read RAM available
        let ram_available_percent = self.read_ram_available()?;

        // Update metrics
        {
            let mut metrics = self.metrics.write();
            metrics.cpu_percent = cpu_percent;
            metrics.ram_available_percent = ram_available_percent;
            metrics.timestamp = now;
        }

        // Update admission mode
        let new_mode = if cpu_percent > self.cpu_threshold || ram_available_percent < self.ram_threshold {
            AdmissionMode::Strict
        } else {
            AdmissionMode::Normal
        };

        *self.mode.write() = new_mode;
        *self.last_update.write() = now;

        // Persist load trend for historical analysis
        if let Some(ref kv_store) = self.kv_store {
            // Note: tx_count would be passed from pool, simplified here
            let _ = kv_store.put_system_load_trend(now, cpu_percent, ram_available_percent, 0);
        }

        Ok(())
    }

    /// Get current admission mode
    pub fn current_mode(&self) -> AdmissionMode {
        self.mode.read().clone()
    }

    /// Get current system metrics
    pub fn current_metrics(&self) -> SystemMetrics {
        self.metrics.read().clone()
    }

    /// Check if transaction should be admitted based on current resources
    /// Returns (should_admit, required_fee_multiplier)
    pub fn should_admit_transaction(&self, base_fee_rate: u64) -> (bool, u64) {
        // Always update metrics before checking
        let _ = self.update_metrics();

        match self.current_mode() {
            AdmissionMode::Normal => (true, 1),
            AdmissionMode::Strict => {
                // In strict mode, require 2x fee
                (true, 2)
            }
        }
    }

    /// Read CPU usage from /proc/stat (Linux-specific)
    fn read_cpu_usage(&self) -> Result<f64, String> {
        let stat_content = fs::read_to_string("/proc/stat")
            .map_err(|e| format!("Failed to read /proc/stat: {}", e))?;

        // Parse first line for total CPU stats
        let first_line = stat_content.lines()
            .next()
            .ok_or("No CPU stats available")?;

        let parts: Vec<&str> = first_line.split_whitespace().collect();
        if parts.len() < 8 || !parts[0].starts_with("cpu") {
            return Err("Invalid /proc/stat format".to_string());
        }

        // Parse CPU times (user, nice, system, idle, iowait, irq, softirq, steal)
        let user: u64 = parts[1].parse().unwrap_or(0);
        let nice: u64 = parts[2].parse().unwrap_or(0);
        let system: u64 = parts[3].parse().unwrap_or(0);
        let idle: u64 = parts[4].parse().unwrap_or(0);
        let iowait: u64 = parts[5].parse().unwrap_or(0);
        let irq: u64 = parts[6].parse().unwrap_or(0);
        let softirq: u64 = parts[7].parse().unwrap_or(0);
        let steal: u64 = parts[8].parse().unwrap_or(0);

        let total_idle = idle + iowait;
        let total_non_idle = user + nice + system + irq + softirq + steal;
        let total = total_idle + total_non_idle;

        if total == 0 {
            return Ok(0.0);
        }

        // Calculate usage percentage (simplified - in real implementation,
        // you'd need to track previous values for accurate delta calculation)
        let usage_percent = (total_non_idle as f64 / total as f64) * 100.0;

        Ok(usage_percent.min(100.0))
    }

    /// Read available RAM from /proc/meminfo (Linux-specific)
    fn read_ram_available(&self) -> Result<f64, String> {
        let meminfo_content = fs::read_to_string("/proc/meminfo")
            .map_err(|e| format!("Failed to read /proc/meminfo: {}", e))?;

        let mut total_mem = 0u64;
        let mut available_mem = 0u64;

        for line in meminfo_content.lines() {
            if line.starts_with("MemTotal:") {
                total_mem = Self::parse_mem_value(line)?;
            } else if line.starts_with("MemAvailable:") {
                available_mem = Self::parse_mem_value(line)?;
            }
        }

        if total_mem == 0 {
            return Ok(100.0);
        }

        let available_percent = (available_mem as f64 / total_mem as f64) * 100.0;
        Ok(available_percent.min(100.0))
    }

    /// Parse memory value from /proc/meminfo line
    fn parse_mem_value(line: &str) -> Result<u64, String> {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            return Ok(0);
        }

        parts[1].parse().map_err(|_| "Invalid memory value".to_string())
    }
}

impl Default for AdmissionController {
    fn default() -> Self {
        Self::new()
    }
}