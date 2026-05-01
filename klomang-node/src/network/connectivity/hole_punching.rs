use libp2p::dcutr::{Behaviour as DcUtRBehaviour, Event as DcUtREvent};
use libp2p::relay::client::{Behaviour as RelayClientBehaviour, Event as RelayClientEvent};
use libp2p::PeerId;

/// Hole punching configuration for direct connection upgrades.
#[derive(Clone, Debug)]
pub struct HolePunchingConfig {
    pub use_relay_client: bool,
}

impl Default for HolePunchingConfig {
    fn default() -> Self {
        Self {
            use_relay_client: true,
        }
    }
}

/// Hole punching manager that combines DCUtR and relay client behaviour.
pub struct HolePunchingManager {
    dcutr: DcUtRBehaviour,
    relay_client: RelayClientBehaviour,
}

impl HolePunchingManager {
    pub fn new(local_peer_id: PeerId, _config: HolePunchingConfig) -> Self {
        let dcutr = DcUtRBehaviour::new();
        let relay_client = RelayClientBehaviour::new(local_peer_id);

        Self { dcutr, relay_client }
    }

    pub fn into_dcutr_behaviour(self) -> DcUtRBehaviour {
        self.dcutr
    }

    pub fn into_relay_behaviour(self) -> RelayClientBehaviour {
        self.relay_client
    }

    pub fn handle_event(
        &mut self,
        event: RelayClientEvent,
    ) -> Option<RelayClientEvent> {
        Some(event)
    }

    pub fn handle_dcutr_event(&mut self, event: DcUtREvent) -> Option<DcUtREvent> {
        Some(event)
    }
}
