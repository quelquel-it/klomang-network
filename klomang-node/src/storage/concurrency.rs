use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rayon::ThreadPoolBuilder;

use crate::storage::batch::WriteBatch;
use crate::storage::cf::ColumnFamilyName;
use crate::storage::db::StorageDb;
use crate::storage::error::{StorageError, StorageResult};

const GROUP_COMMIT_MAX_REQUESTS: usize = 16;
#[allow(dead_code)]
const GROUP_COMMIT_WAIT_MS: u64 = 10;

/// A single write command that can be executed against RocksDB.
#[derive(Debug)]
pub enum StorageWriteCommand {
    Put {
        cf: ColumnFamilyName,
        key: Vec<u8>,
        value: Vec<u8>,
    },
    Delete {
        cf: ColumnFamilyName,
        key: Vec<u8>,
    },
}

#[derive(Debug)]
struct StorageWriteMessage {
    commands: Vec<StorageWriteCommand>,
    response: Option<SyncSender<Result<(), StorageError>>>,
}

impl StorageWriteMessage {
    fn append_to(&self, batch: &mut WriteBatch) {
        for command in &self.commands {
            match command {
                StorageWriteCommand::Put { cf, key, value } => {
                    batch.put_cf_typed(*cf, key, value);
                }
                StorageWriteCommand::Delete { cf, key } => {
                    batch.delete_cf_typed(*cf, key);
                }
            }
        }
    }
}

/// Dedicated writer thread that processes all RocksDB write requests sequentially.
#[derive(Clone, Debug)]
pub struct StorageWriter {
    sender: Sender<StorageWriteMessage>,
    pub pending_writes: Arc<AtomicU64>,
    pub committed_batches: Arc<AtomicU64>,
    pub is_shutting_down: Arc<AtomicBool>,
}

impl StorageWriter {
    pub fn new(db: Arc<StorageDb>) -> Self {
        let (sender, receiver) = mpsc::channel();
        let pending_writes = Arc::new(AtomicU64::new(0));
        let committed_batches = Arc::new(AtomicU64::new(0));
        let is_shutting_down = Arc::new(AtomicBool::new(false));

        let worker_pending = Arc::clone(&pending_writes);
        let worker_committed = Arc::clone(&committed_batches);
        let worker_shutdown = Arc::clone(&is_shutting_down);
        thread::Builder::new()
            .name("storage-writer".into())
            .spawn(move || {
                Self::writer_loop(receiver, db, worker_pending, worker_committed, worker_shutdown);
            })
            .expect("failed to spawn storage writer thread");

        Self {
            sender,
            pending_writes,
            committed_batches,
            is_shutting_down,
        }
    }

    pub fn enqueue<I>(&self, commands: I) -> Result<(), StorageError>
    where
        I: IntoIterator<Item = StorageWriteCommand>,
    {
        let command_list: Vec<StorageWriteCommand> = commands.into_iter().collect();
        if command_list.is_empty() {
            return Ok(());
        }

        let (response_tx, response_rx) = mpsc::sync_channel(1);
        let message = StorageWriteMessage {
            commands: command_list,
            response: Some(response_tx),
        };

        self.sender
            .send(message)
            .map_err(|_| StorageError::OperationFailed("write queue has been closed".into()))?;

        self.pending_writes.fetch_add(1, Ordering::Relaxed);

        response_rx
            .recv()
            .map_err(|_| StorageError::OperationFailed("storage writer thread terminated".into()))?
    }

    fn writer_loop(
        receiver: Receiver<StorageWriteMessage>,
        db: Arc<StorageDb>,
        pending_writes: Arc<AtomicU64>,
        committed_batches: Arc<AtomicU64>,
        is_shutting_down: Arc<AtomicBool>,
    ) {
        while let Ok(first_message) = receiver.recv() {
            pending_writes.fetch_sub(1, Ordering::Relaxed);

            let mut combined_batch = WriteBatch::new();
            let mut responders = Vec::new();
            first_message.append_to(&mut combined_batch);
            if let Some(response) = first_message.response {
                responders.push(response);
            }

            for _ in 1..GROUP_COMMIT_MAX_REQUESTS {
                match receiver.try_recv() {
                    Ok(message) => {
                        pending_writes.fetch_sub(1, Ordering::Relaxed);
                        message.append_to(&mut combined_batch);
                        if let Some(response) = message.response {
                            responders.push(response);
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break,
                }
            }

            let result = db
                .write_batch(combined_batch)
                .map_err(StorageError::from);

            if result.is_ok() {
                committed_batches.fetch_add(1, Ordering::Relaxed);
            }

            for responder in responders {
                let _ = responder.send(result.clone());
            }

            // Wait for more writes if queue is being filled slowly
            // (GROUP_COMMIT_WAIT_MS ensures we don't wait indefinitely)
            thread::sleep(Duration::from_millis(1));
        }

        is_shutting_down.store(true, Ordering::Release);
    }
}

/// A read executor that sends read workloads to a Rayon thread pool.
#[derive(Clone, Debug)]
pub struct StorageReadExecutor {
    pool: Arc<rayon::ThreadPool>,
}

impl StorageReadExecutor {
    pub fn new(thread_count: Option<usize>) -> StorageResult<Self> {
        let num_threads = thread_count.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|value| value.get())
                .unwrap_or(4)
        });

        let pool = ThreadPoolBuilder::new()
            .num_threads(num_threads)
            .thread_name(|i| format!("storage-reader-{}", i))
            .build()
            .map_err(|e| StorageError::OperationFailed(format!("failed to build read pool: {}", e)))?;

        Ok(Self {
            pool: Arc::new(pool),
        })
    }

    pub fn read<T, F>(&self, operation: F) -> StorageResult<T>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.pool.spawn(move || {
            let result = operation();
            let _ = sender.send(result);
        });

        receiver
            .recv()
            .map_err(|_| StorageError::OperationFailed("read worker thread terminated".into()))
    }
}

/// A combined storage engine exposing a thread-pooled read path and a write queue.
#[derive(Clone, Debug)]
pub struct StorageEngine {
    pub cache_layer: Arc<crate::storage::cache::StorageCacheLayer>,
    pub reader: StorageReadExecutor,
    pub writer: Arc<StorageWriter>,
}

impl StorageEngine {
    pub fn new(db: StorageDb) -> StorageResult<Self> {
        let db = Arc::new(db);
        let writer = Arc::new(StorageWriter::new(Arc::clone(&db)));
        let cache_layer = Arc::new(crate::storage::cache::StorageCacheLayer::new_with_writer(db, Arc::clone(&writer)));
        let reader = StorageReadExecutor::new(None)?;

        Ok(Self {
            cache_layer,
            reader,
            writer,
        })
    }
}
