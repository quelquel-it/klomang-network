use crate::core::errors::CoreError;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub network: String,
    pub data_dir: String,
    pub max_block_weight: u64,
    pub mempool_max_size: usize,
    pub block_reward: u64,
    pub k: usize,
    pub target_block_time: u64,
    pub finality_depth: usize,
    // Hardware parameters for storage optimization
    pub num_cpus: usize,
    pub total_memory_mb: usize,
    pub disk_write_bandwidth_mbps: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            network: "mainnet".to_string(),
            data_dir: "./data".to_string(),
            max_block_weight: 4_000_000,
            mempool_max_size: 10000,
            num_cpus: num_cpus::get(),
            total_memory_mb: (sys_info::mem_info().unwrap().total / 1024 / 1024) as usize,
            disk_write_bandwidth_mbps: 100, // Default assumption, can be configured
            block_reward: 100,
            k: 18,
            target_block_time: 1,
            finality_depth: 100,
        }
    }
}

impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load_config(_path: &str) -> Result<Config, CoreError> {
        // In pure library mode, config is deterministic and defaulted.
        // Path-based configuration is not needed in this stateless core.
        Ok(Config::default())
    }
}
