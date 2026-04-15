use std::fmt;

#[derive(Debug, Clone)]
pub enum StorageError {
    DbError(String),
    NotFound(String),
    InvalidColumnFamily(String),
    OperationFailed(String),
    ConfigError(String),
    SerializationError(String),
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageError::DbError(msg) => write!(f, "database error: {}", msg),
            StorageError::NotFound(msg) => write!(f, "not found: {}", msg),
            StorageError::InvalidColumnFamily(name) => write!(f, "invalid column family: {}", name),
            StorageError::OperationFailed(msg) => write!(f, "operation failed: {}", msg),
            StorageError::ConfigError(msg) => write!(f, "config error: {}", msg),
            StorageError::SerializationError(msg) => write!(f, "serialization error: {}", msg),
        }
    }
}

impl std::error::Error for StorageError {}

impl From<rocksdb::Error> for StorageError {
    fn from(err: rocksdb::Error) -> Self {
        StorageError::DbError(err.to_string())
    }
}

impl From<bincode::Error> for StorageError {
    fn from(err: bincode::Error) -> Self {
        StorageError::SerializationError(err.to_string())
    }
}

pub type StorageResult<T> = Result<T, StorageError>;
