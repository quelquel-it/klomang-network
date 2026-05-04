use libp2p::floodsub::{Floodsub, FloodsubEvent, Topic};
use libp2p::PeerId;

/// Configuration for Floodsub critical messaging
#[derive(Clone, Debug)]
pub struct FloodsubConfig {
    pub bootstrap_topic: String,
    pub emergency_topic: String,
}

impl Default for FloodsubConfig {
    fn default() -> Self {
        Self {
            bootstrap_topic: "klomang/bootstrap/v1".to_string(),
            emergency_topic: "klomang/emergency/v1".to_string(),
        }
    }
}

/// Main topics for Floodsub critical messaging
pub struct FloodsubTopics;

impl FloodsubTopics {
    pub const BOOTSTRAP: &'static str = "klomang/bootstrap/v1";
    pub const EMERGENCY: &'static str = "klomang/emergency/v1";

    pub fn bootstrap_topic() -> Topic {
        Topic::new(Self::BOOTSTRAP)
    }

    pub fn emergency_topic() -> Topic {
        Topic::new(Self::EMERGENCY)
    }
}

/// Build Floodsub behaviour for critical messages
pub fn build_floodsub_behaviour(local_peer_id: PeerId) -> Floodsub {
    Floodsub::new(local_peer_id)
}

/// Subscribe to a Floodsub topic
pub fn subscribe_to_floodsub_topic(behaviour: &mut Floodsub, topic: Topic) {
    behaviour.subscribe(topic);
}

/// Publish a critical message to Floodsub
pub fn publish_floodsub_message(behaviour: &mut Floodsub, topic: Topic, data: Vec<u8>) {
    behaviour.publish(topic, data);
}

/// Handle Floodsub events
pub fn handle_floodsub_event(event: FloodsubEvent) -> Option<Vec<u8>> {
    match event {
        FloodsubEvent::Message(message) => Some(message.data.to_vec()),
        FloodsubEvent::Subscribed { .. } => None,
        FloodsubEvent::Unsubscribed { .. } => None,
    }
}
