use crate::storage::cf::ColumnFamilyName;
use crate::storage::db::StorageDb;
use bincode;
use libp2p::gossipsub::MessageId;
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::time::{Duration, Instant};

const CACHE_STORE_KEY: &[u8] = b"gossipsub_cache:entries";

/// Cache of recently processed Gossipsub message IDs with optional persistence.
pub struct GossipsubCache {
    entries: HashMap<MessageId, Instant>,
    order: VecDeque<MessageId>,
    capacity: usize,
    ttl: Duration,
    storage_db: Option<StorageDb>,
}

impl GossipsubCache {
    /// Create a new in-memory cache with a TTL and optional persistent store.
    pub fn new(capacity: usize, ttl: Duration, storage_db: Option<StorageDb>) -> Self {
        let mut cache = Self {
            entries: HashMap::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            capacity,
            ttl,
            storage_db,
        };

        cache.load_persistent_entries();
        cache
    }

    /// Create a new cache backed by `StorageDb` persistence.
    pub fn with_storage<P: AsRef<Path>>(capacity: usize, ttl: Duration, storage_path: P) -> Self {
        let storage_db = StorageDb::new(storage_path.as_ref()).ok();
        Self::new(capacity, ttl, storage_db)
    }

    /// Check if a message ID has already been processed.
    pub fn contains(&mut self, message_id: &MessageId) -> bool {
        self.prune_expired();
        self.entries.contains_key(message_id)
    }

    /// Insert a message ID into the cache and persist it if persistence is enabled.
    pub fn insert(&mut self, message_id: MessageId) {
        if self.contains(&message_id) {
            return;
        }

        self.insert_entry(message_id, true);
    }

    fn insert_entry(&mut self, message_id: MessageId, persist: bool) {
        self.prune_expired();

        if self.entries.contains_key(&message_id) {
            return;
        }

        self.entries.insert(message_id.clone(), Instant::now());
        self.order.push_back(message_id.clone());

        while self.order.len() > self.capacity {
            if let Some(old_id) = self.order.pop_front() {
                self.entries.remove(&old_id);
            }
        }

        if persist {
            self.persist().ok();
        }
    }

    fn prune_expired(&mut self) {
        let now = Instant::now();
        while let Some(front_id) = self.order.front() {
            if let Some(inserted_at) = self.entries.get(front_id) {
                if now.duration_since(*inserted_at) > self.ttl {
                    let old_id = self.order.pop_front().unwrap();
                    self.entries.remove(&old_id);
                    continue;
                }
            }
            break;
        }
    }

    fn persist(&self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(db) = &self.storage_db {
            let ids: Vec<Vec<u8>> = self.order.iter().map(|id| id.0.clone()).collect();
            let encoded = bincode::serialize(&ids)?;
            db.put(ColumnFamilyName::Default, CACHE_STORE_KEY, &encoded)?;
        }
        Ok(())
    }

    fn load_persistent_entries(&mut self) {
        if let Some(db) = &self.storage_db {
            if let Ok(Some(bytes)) = db.get(ColumnFamilyName::Default, CACHE_STORE_KEY) {
                if let Ok(keys) = bincode::deserialize::<Vec<Vec<u8>>>(&bytes) {
                    for message_bytes in keys.into_iter().take(self.capacity) {
                        let message_id = MessageId::from(message_bytes);
                        self.entries.insert(message_id.clone(), Instant::now());
                        self.order.push_back(message_id);
                    }
                }
            }
        }
    }
}
