use rocksdb::WriteBatch as RocksWriteBatch;
use crate::storage::cf::ColumnFamilyName;

#[derive(Debug)]
pub struct WriteBatch {
    inner: RocksWriteBatch,
}

impl WriteBatch {
    pub fn new() -> Self {
        Self {
            inner: RocksWriteBatch::default(),
        }
    }

    pub fn put(&mut self, key: &[u8], value: &[u8]) {
        self.inner.put(key, value);
    }

    pub fn delete(&mut self, key: &[u8]) {
        self.inner.delete(key);
    }

    pub fn put_cf(&mut self, cf: &str, key: &[u8], value: &[u8]) {
        self.inner.put_cf(cf, key, value);
    }

    pub fn delete_cf(&mut self, cf: &str, key: &[u8]) {
        self.inner.delete_cf(cf, key);
    }

    /// Strongly-typed put operation for named column families
    pub fn put_cf_typed(&mut self, cf: ColumnFamilyName, key: &[u8], value: &[u8]) {
        self.inner.put_cf(cf.as_str(), key, value);
    }

    /// Strongly-typed delete operation for named column families
    pub fn delete_cf_typed(&mut self, cf: ColumnFamilyName, key: &[u8]) {
        self.inner.delete_cf(cf.as_str(), key);
    }

    pub fn into_inner(self) -> RocksWriteBatch {
        self.inner
    }
}
