use libp2p::yamux;
use std::time::Duration;

/// Logical stream categories supported by the Klomang network multiplexer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamType {
    Transactions,
    BlockSync,
    PeerExchange,
}

impl StreamType {
    /// Protocol name used to distinguish streams over the multiplexed connection.
    pub fn protocol_name(&self) -> &'static [u8] {
        match self {
            StreamType::Transactions => b"/klomang/transactions/1.0.0",
            StreamType::BlockSync => b"/klomang/blocks/1.0.0",
            StreamType::PeerExchange => b"/klomang/peer-exchange/1.0.0",
        }
    }

    /// QUIC-native stream label for environments that support QUIC streams directly.
    pub fn quic_stream_label(&self) -> &'static str {
        match self {
            StreamType::Transactions => "klomang/transactions",
            StreamType::BlockSync => "klomang/blocks",
            StreamType::PeerExchange => "klomang/peer-exchange",
        }
    }
}

/// Configuration for the network multiplexer.
#[derive(Clone, Debug)]
pub struct MultiplexerConfig {
    pub max_streams: usize,
    pub idle_timeout: Duration,
}

impl Default for MultiplexerConfig {
    fn default() -> Self {
        Self {
            max_streams: 64,
            idle_timeout: Duration::from_secs(120),
        }
    }
}

/// Yamux-based multiplexer wrapper for the Klomang networking layer.
#[derive(Clone, Debug)]
pub struct CoreMultiplexer {
    config: MultiplexerConfig,
}

impl CoreMultiplexer {
    pub fn new(config: MultiplexerConfig) -> Self {
        Self { config }
    }

    pub fn default() -> Self {
        Self {
            config: MultiplexerConfig::default(),
        }
    }

    /// Configure yamux with limits that help prevent OOM by bounding streams.
    pub fn yamux_config(&self) -> yamux::Config {
        let mut config = yamux::Config::default();
        config.set_max_num_streams(self.config.max_streams);
        config
    }

    pub fn max_streams(&self) -> usize {
        self.config.max_streams
    }
}
