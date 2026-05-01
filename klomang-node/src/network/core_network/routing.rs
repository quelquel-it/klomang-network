use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use libp2p::{Multiaddr, PeerId};
use rocksdb::{Options, WriteBatch, DB};
use serde::{Deserialize, Serialize};

use crate::storage::error::{StorageError, StorageResult};

/// Maximum number of peers to store in the routing table (anti-OOM protection).
const MAX_ROUTING_TABLE_SIZE: usize = 1000;

/// Time-to-live for peer records in seconds (7 days).
const PEER_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;

/// Stored peer information with metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerRecord {
    pub peer_id_bytes: Vec<u8>, // Store PeerId as bytes
    pub addresses: Vec<Multiaddr>,
    pub last_seen: u64, // Unix timestamp
    pub connection_count: u32,
    pub is_bootstrap: bool,
}

impl PeerRecord {
    /// Create a new peer record.
    pub fn new(peer_id: PeerId, addresses: Vec<Multiaddr>, is_bootstrap: bool) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            peer_id_bytes: peer_id.to_bytes(),
            addresses,
            last_seen: now,
            connection_count: 1,
            is_bootstrap,
        }
    }

    /// Get the PeerId from stored bytes.
    pub fn peer_id(&self) -> Result<PeerId, String> {
        PeerId::from_bytes(&self.peer_id_bytes).map_err(|e| e.to_string())
    }

    /// Update the last seen timestamp and increment connection count.
    pub fn update_seen(&mut self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.last_seen = now;
        self.connection_count = self.connection_count.saturating_add(1);
    }

    /// Check if this peer record has expired.
    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        now.saturating_sub(self.last_seen) > PEER_TTL_SECONDS
    }

    /// Get the primary address for this peer.
    pub fn primary_address(&self) -> Option<&Multiaddr> {
        self.addresses.first()
    }
}

/// Persistent peer routing table using RocksDB.
pub struct PeerRoutingTable {
    db: Arc<DB>,
    cache: HashMap<PeerId, PeerRecord>,
}

impl PeerRoutingTable {
    /// Open or create a peer routing table database.
    pub fn open(path: &std::path::Path) -> StorageResult<Self> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.set_max_open_files(1000);

        let db = Arc::new(DB::open(&opts, path).map_err(StorageError::from)?);

        // Load existing peers from database
        let mut cache = HashMap::new();
        let iter = db.iterator(rocksdb::IteratorMode::Start);

        for item in iter {
            let (key, value) = item.map_err(StorageError::from)?;
            if let Ok(peer_id) = PeerId::from_bytes(&key) {
                if let Ok(record) = bincode::deserialize::<PeerRecord>(&value) {
                    // Only load non-expired records
                    if !record.is_expired() {
                        cache.insert(peer_id, record);
                    }
                }
            }
        }

        Ok(Self { db, cache })
    }

    /// Add or update a peer in the routing table.
    pub fn add_peer(
        &mut self,
        peer_id: PeerId,
        addresses: Vec<Multiaddr>,
        is_bootstrap: bool,
    ) -> StorageResult<()> {
        // Check if we're at capacity and need to evict old peers
        if self.cache.len() >= MAX_ROUTING_TABLE_SIZE && !self.cache.contains_key(&peer_id) {
            self.evict_oldest_peer()?;
        }

        let record = self
            .cache
            .entry(peer_id)
            .or_insert_with(|| PeerRecord::new(peer_id, addresses.clone(), is_bootstrap));

        record.update_seen();
        if !addresses.is_empty() {
            // Merge addresses, keeping unique ones
            let mut all_addresses = record.addresses.clone();
            for addr in addresses {
                if !all_addresses.contains(&addr) {
                    all_addresses.push(addr);
                }
            }
            record.addresses = all_addresses;
        }

        // Persist to database
        let key = peer_id.to_bytes();
        let value = bincode::serialize(record)
            .map_err(|e| StorageError::SerializationError(e.to_string()))?;
        self.db.put(&key, &value).map_err(StorageError::from)?;

        Ok(())
    }

    /// Remove a peer from the routing table.
    pub fn remove_peer(&mut self, peer_id: &PeerId) -> StorageResult<()> {
        self.cache.remove(peer_id);

        let key = peer_id.to_bytes();
        self.db.delete(&key).map_err(StorageError::from)?;

        Ok(())
    }

    /// Get a peer record by ID.
    pub fn get_peer(&self, peer_id: &PeerId) -> Option<&PeerRecord> {
        self.cache.get(peer_id)
    }

    /// Get all known peers.
    pub fn get_all_peers(&self) -> Vec<&PeerRecord> {
        self.cache.values().collect()
    }

    /// Get bootstrap peers only.
    pub fn get_bootstrap_peers(&self) -> Vec<&PeerRecord> {
        self.cache.values().filter(|r| r.is_bootstrap).collect()
    }

    /// Get recently seen peers (within last hour).
    pub fn get_recent_peers(&self) -> Vec<&PeerRecord> {
        let one_hour_ago = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_sub(3600);

        self.cache
            .values()
            .filter(|r| r.last_seen >= one_hour_ago)
            .collect()
    }

    /// Clean up expired peers from both cache and database.
    pub fn cleanup_expired(&mut self) -> StorageResult<()> {
        let expired_peers: Vec<PeerId> = self
            .cache
            .iter()
            .filter(|(_, record)| record.is_expired())
            .map(|(peer_id, _)| *peer_id)
            .collect();

        if expired_peers.is_empty() {
            return Ok(());
        }

        let mut batch = WriteBatch::default();
        for peer_id in &expired_peers {
            self.cache.remove(peer_id);
            batch.delete(&peer_id.to_bytes());
        }

        self.db.write(batch).map_err(StorageError::from)?;
        println!("Cleaned up {} expired peer records", expired_peers.len());

        Ok(())
    }

    /// Get the current size of the routing table.
    pub fn size(&self) -> usize {
        self.cache.len()
    }

    /// Check if the routing table is at capacity.
    pub fn is_at_capacity(&self) -> bool {
        self.cache.len() >= MAX_ROUTING_TABLE_SIZE
    }

    /// Evict the oldest peer to make room for new ones.
    fn evict_oldest_peer(&mut self) -> StorageResult<()> {
        if let Some((oldest_peer_id, _)) =
            self.cache.iter().min_by_key(|(_, record)| record.last_seen)
        {
            let peer_id = *oldest_peer_id;
            self.remove_peer(&peer_id)?;
            println!(
                "Evicted oldest peer {} to maintain routing table size",
                peer_id
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_peer_routing_table_basic() {
        let temp_dir = TempDir::new().unwrap();
        let mut table = PeerRoutingTable::open(temp_dir.path()).unwrap();

        let peer_id = PeerId::random();
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().unwrap();

        table.add_peer(peer_id, vec![addr.clone()], false).unwrap();

        let record = table.get_peer(&peer_id).unwrap();
        assert_eq!(record.peer_id, peer_id);
        assert!(record.addresses.contains(&addr));
        assert_eq!(record.connection_count, 1);
    }

    #[test]
    fn test_routing_table_capacity() {
        let temp_dir = TempDir::new().unwrap();
        let mut table = PeerRoutingTable::open(temp_dir.path()).unwrap();

        // Fill up to capacity
        for i in 0..MAX_ROUTING_TABLE_SIZE + 10 {
            let peer_id = PeerId::random();
            let addr: Multiaddr = format!("/ip4/127.0.0.1/tcp/{}", 4000 + i).parse().unwrap();
            table.add_peer(peer_id, vec![addr], false).unwrap();
        }

        // Should not exceed max size
        assert!(table.size() <= MAX_ROUTING_TABLE_SIZE);
    }
}
