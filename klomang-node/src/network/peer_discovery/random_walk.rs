use std::time::{Duration, Instant};

use libp2p::PeerId;

/// Random walk configuration for periodic Kademlia discovery.
#[derive(Clone, Debug)]
pub struct RandomWalkConfig {
    pub interval: Duration,
}

impl Default for RandomWalkConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(600),
        }
    }
}

impl RandomWalkConfig {
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(seconds) = std::env::var("KL_MANG_RANDOM_WALK_INTERVAL_SECS") {
            if let Ok(value) = seconds.parse::<u64>() {
                config.interval = Duration::from_secs(value.max(60));
            }
        }

        config
    }
}

/// Controller for periodic Kademlia random walks.
pub struct RandomWalkControl {
    pub config: RandomWalkConfig,
    last_walk: Instant,
}

impl RandomWalkControl {
    pub fn new(config: RandomWalkConfig) -> Self {
        let interval = config.interval;
        Self {
            config,
            last_walk: Instant::now() - interval,
        }
    }

    /// Determine whether a random walk should be performed now.
    pub fn should_walk(&self) -> bool {
        self.last_walk.elapsed() >= self.config.interval
    }

    /// Schedule the next random-walk target and reset the timer.
    pub fn next_target(&mut self) -> PeerId {
        self.last_walk = Instant::now();
        PeerId::random()
    }
}
