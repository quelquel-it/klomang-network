use std::collections::HashMap;

/// Pure in-memory storage abstraction for Klomang Core.
pub trait Storage {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>>;
    fn put(&mut self, key: Vec<u8>, value: Vec<u8>);
    fn delete(&mut self, key: &[u8]);
    fn clear(&mut self);
}

/// Simple in-memory key/value storage implementation.
#[derive(Debug, Clone)]
pub struct MemoryStorage {
    pub map: HashMap<Vec<u8>, Vec<u8>>,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }
}

impl Default for MemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl Storage for MemoryStorage {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.map.get(key).cloned()
    }

    fn put(&mut self, key: Vec<u8>, value: Vec<u8>) {
        self.map.insert(key, value);
    }

    fn delete(&mut self, key: &[u8]) {
        self.map.remove(key);
    }

    fn clear(&mut self) {
        self.map.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_storage_put_get_delete() {
        let mut storage = MemoryStorage::new();
        let key = b"test-key".to_vec();
        let value = b"test-value".to_vec();

        assert!(storage.get(&key).is_none());
        storage.put(key.clone(), value.clone());
        assert_eq!(storage.get(&key), Some(value.clone()));
        storage.delete(&key);
        assert!(storage.get(&key).is_none());
    }
}
