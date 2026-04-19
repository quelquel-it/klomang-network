use crate::storage::cf::ColumnFamilyName;

/// Represents deferred write operations
#[derive(Clone, Debug)]
pub enum WriteOp {
    Put(Vec<u8>, Vec<u8>),
    Delete(Vec<u8>),
    PutCf(String, Vec<u8>, Vec<u8>),
    DeleteCf(String, Vec<u8>),
}

pub struct WriteBatch {
    ops: Vec<WriteOp>,
}

impl std::fmt::Debug for WriteBatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteBatch")
            .field("ops_count", &self.ops.len())
            .finish()
    }
}

impl WriteBatch {
    pub fn new() -> Self {
        Self { ops: Vec::new() }
    }

    pub fn put(&mut self, key: &[u8], value: &[u8]) {
        self.ops.push(WriteOp::Put(key.to_vec(), value.to_vec()));
    }

    pub fn delete(&mut self, key: &[u8]) {
        self.ops.push(WriteOp::Delete(key.to_vec()));
    }

    pub fn put_cf(&mut self, cf: &str, key: &[u8], value: &[u8]) {
        self.ops.push(WriteOp::PutCf(cf.to_string(), key.to_vec(), value.to_vec()));
    }

    pub fn delete_cf(&mut self, cf: &str, key: &[u8]) {
        self.ops.push(WriteOp::DeleteCf(cf.to_string(), key.to_vec()));
    }

    /// Strongly-typed put operation for named column families
    pub fn put_cf_typed(&mut self, cf: ColumnFamilyName, key: &[u8], value: &[u8]) {
        self.ops.push(WriteOp::PutCf(cf.as_str().to_string(), key.to_vec(), value.to_vec()));
    }

    /// Strongly-typed delete operation for named column families
    pub fn delete_cf_typed(&mut self, cf: ColumnFamilyName, key: &[u8]) {
        self.ops.push(WriteOp::DeleteCf(cf.as_str().to_string(), key.to_vec()));
    }

    /// Convert deferred operations to a RocksDB WriteBatch
    /// This is called by StorageDb::write_batch after resolving CF handles
    pub(crate) fn into_inner(self) -> Vec<WriteOp> {
        self.ops
    }
}
