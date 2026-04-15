# Sistem Scheduler Klomang Core

## Pendahuluan

Klomang Core mengimplementasikan sistem scheduler paralel yang canggih untuk optimisasi eksekusi transaksi dalam blok. Scheduler ini menggunakan analisis access set untuk mendeteksi konflik antar transaksi dan mengelompokkannya ke dalam batch paralel yang dapat dieksekusi secara simultan. Sistem ini dirancang untuk memaksimalkan throughput transaksi sambil mempertahankan konsistensi state dan atomicity. Dokumen ini menyajikan analisis mendalam tentang mekanisme scheduling paralel yang digunakan dalam Klomang Core.

## 1. Arsitektur Scheduler Paralel

### 1.1 ScheduledTransaction Structure

Struktur utama untuk merepresentasikan transaksi yang dijadwalkan.

```rust
pub struct ScheduledTransaction {
    pub tx: Transaction,
    pub access_set: AccessSet,
    pub index: usize, // For deterministic ordering
}
```

#### Komponen Utama
- **Transaction**: Objek transaksi lengkap
- **Access Set**: Set key yang diakses (read/write) oleh transaksi
- **Index**: Indeks untuk ordering deterministik

### 1.2 ParallelScheduler Implementation

Scheduler utama yang mengelola pengelompokan dan eksekusi transaksi paralel.

```rust
pub struct ParallelScheduler;
```

#### Metode Utama
- **schedule_transactions()**: Mengelompokkan transaksi ke dalam batch paralel
- **execute_groups()**: Mengeksekusi grup dengan integrasi StateManager

## 2. Algoritma Scheduling

### 2.1 Analisis Access Set

Setiap transaksi dianalisis untuk menentukan key state yang diakses.

```rust
pub fn generate_access_set(&self) -> AccessSet
```
- **Read Set**: Key yang dibaca transaksi
- **Write Set**: Key yang ditulis transaksi
- **Automatic Generation**: Access set dihasilkan otomatis dari struktur transaksi

### 2.2 Conflict Detection

Deteksi konflik antar transaksi berdasarkan access patterns.

```rust
pub fn has_conflict(&self, other: &AccessSet) -> bool {
    // Conflict if any write set overlaps with any read or write set of other
    !self.write_set.is_disjoint(&other.read_set) ||
    !self.write_set.is_disjoint(&other.write_set) ||
    !other.write_set.is_disjoint(&self.read_set)
}
```

#### Tipe Konflik
- **Read-Write Conflict**: Transaksi A membaca key yang ditulis transaksi B
- **Write-Write Conflict**: Kedua transaksi menulis ke key yang sama
- **Cross-Transaction Conflict**: Konflik tidak langsung melalui chain dependencies

### 2.3 Grouping Algorithm

Algoritma greedy untuk pengelompokan transaksi non-konflik.

```rust
pub fn schedule_transactions(txs: Vec<Transaction>) -> Vec<Vec<ScheduledTransaction>>
```

#### Langkah-Langkah
1. **Initialization**: Semua transaksi dimasukkan ke queue
2. **Group Formation**: Iteratif membentuk grup non-konflik
3. **Conflict Resolution**: Jika tidak ada kandidat non-konflik, ambil transaksi pertama
4. **Deterministic Ordering**: Sort berdasarkan indeks asli untuk konsistensi

#### Optimisasi
- **Greedy Approach**: Memaksimalkan paralelisme dalam setiap grup
- **Sequential Fallback**: Memastikan progress bahkan dengan konflik tinggi
- **Memory Efficiency**: Menggunakan VecDeque untuk operasi yang efisien

## 3. Mekanisme Eksekusi

### 3.1 Group Conflict Detection

Validasi akhir sebelum eksekusi untuk memastikan tidak ada konflik antar grup.

```rust
pub fn detect_group_conflicts(groups: &[Vec<ScheduledTransaction>]) -> Option<(usize, usize)>
```

#### Cross-Group Validation
- **Inter-Group Conflicts**: Deteksi konflik antara transaksi di grup berbeda
- **Dependency Chains**: Validasi chain dependencies yang kompleks
- **Early Detection**: Gagal cepat jika konflik terdeteksi

### 3.2 Atomic Execution dengan Rollback

Setiap grup dieksekusi secara atomik dengan kemampuan rollback.

```rust
pub fn execute_groups<S: Storage + Clone + Send + Sync + 'static>(
    groups: Vec<Vec<ScheduledTransaction>>,
    state_manager: &mut StateManager<S>,
    utxo: &mut UtxoSet,
) -> Result<(), StateManagerError>
```

#### Backup State
Sebelum eksekusi grup, state lengkap di-backup:
- **Verkle Tree**: Full tree state
- **Height**: Current block height
- **Snapshots**: State snapshots
- **Total Supply**: Current total supply
- **Gas Fees**: Accumulated gas fees
- **UTXO Set**: Complete UTXO state

#### Sequential Execution dalam Grup
```rust
// Execute transactions in the group sequentially to maintain state consistency
for scheduled_tx in group {
    state_manager.apply_transaction(&scheduled_tx.tx, utxo)?;
}
```

- **Intra-Group Sequential**: Transaksi dalam grup dieksekusi berurutan
- **State Consistency**: Memastikan konsistensi state dalam grup
- **Error Handling**: Rollback jika ada transaksi gagal

#### Rollback Mechanism
Jika eksekusi grup gagal, semua perubahan direvert:
- **State Restoration**: Restore dari backup
- **Atomicity Guarantee**: Semua atau tidak sama sekali
- **Error Propagation**: Error dikembalikan ke caller

## 4. Integrasi dengan State Manager

### 4.1 StateManager Integration

Scheduler terintegrasi erat dengan StateManager untuk state transitions.

```rust
state_manager.apply_transaction(&scheduled_tx.tx, utxo)?
```

#### State Transitions
- **Transaction Application**: Setiap transaksi diterapkan ke state
- **Verkle Tree Updates**: Incremental updates ke Verkle tree
- **UTXO Updates**: Update UTXO set sesuai transaksi

### 4.2 Gas Fee Accumulation

Gas fees dari eksekusi transaksi diakumulasikan dalam StateManager.

```rust
state_manager.block_gas_fees.push(gas_fee_witness);
```

- **Fee Collection**: Semua gas fees dikumpulkan
- **80/20 Split**: Distribusi ke miner dan full nodes
- **Witness Recording**: Bukti distribusi untuk audit

## 5. Optimisasi Performa

### 5.1 Paralelisme Maksimal

Scheduler dirancang untuk memaksimalkan paralelisme eksekusi.

#### Trade-offs
- **Throughput vs Latency**: Lebih banyak grup = lebih banyak paralelisme
- **Overhead vs Benefit**: Grup kecil meningkatkan overhead scheduling
- **Conflict Resolution**: Balancing antara paralelisme dan kompleksitas

### 5.2 Memory Management

Optimisasi penggunaan memory selama scheduling.

#### Data Structures
- **VecDeque**: Untuk queue transaksi yang efisien
- **HashSet**: Untuk fast conflict detection
- **Cloning**: Minimal cloning untuk performa

### 5.3 Deterministic Ordering

Memastikan hasil scheduling yang deterministik.

```rust
scheduled.sort_by_key(|s| s.index);
```

- **Reproducibility**: Hasil scheduling selalu sama untuk input sama
- **Debugging**: Memudahkan debugging dan testing
- **Consensus**: Penting untuk konsensus blockchain

## 6. Mekanisme Validasi

### 6.1 Pre-Execution Validation

Validasi sebelum eksekusi untuk mencegah error.

#### Group Validation
- **Conflict Check**: Validasi tidak ada konflik antar grup
- **State Consistency**: Pastikan state dalam kondisi valid
- **Resource Limits**: Validasi batas resource untuk eksekusi

### 6.2 Runtime Error Handling

Penanganan error selama eksekusi.

#### Error Types
- **StateManagerError**: Error dari state transitions
- **VMError**: Error dari smart contract execution
- **GasError**: Out-of-gas atau insufficient balance

#### Recovery Strategies
- **Rollback**: Revert state ke kondisi sebelum grup
- **Partial Success**: Beberapa grup berhasil, beberapa gagal
- **Logging**: Comprehensive logging untuk debugging

## 7. Skalabilitas dan Ekstensibilitas

### 7.1 Horizontal Scaling

Scheduler mendukung scaling ke multiple cores/machines.

#### Multi-Threading
- **Thread-Safe**: Semua operasi thread-safe
- **Concurrent Execution**: Grup dapat dieksekusi di thread berbeda
- **Synchronization**: Koordinasi antar thread untuk state consistency

### 7.2 Adaptive Scheduling

Potensi untuk scheduling adaptif berdasarkan kondisi runtime.

#### Future Extensions
- **Load Balancing**: Distribusi load ke multiple executors
- **Priority Queues**: Scheduling berdasarkan fee atau priority
- **Machine Learning**: Optimisasi scheduling menggunakan ML

### 7.3 Integration dengan DAG Consensus

Scheduler terintegrasi dengan konsensus DAG untuk ordering.

#### Block Processing
- **Transaction Ordering**: Ordering dari konsensus DAG
- **Parallel Validation**: Validasi paralel dalam blok
- **Finality Integration**: Coordination dengan finality rules

## Kesimpulan

Sistem scheduler paralel Klomang Core merupakan implementasi yang inovatif untuk mengoptimalkan throughput transaksi blockchain. Melalui analisis access set yang canggih, algoritma grouping yang efisien, dan mekanisme eksekusi atomik dengan rollback, scheduler ini mencapai keseimbangan optimal antara paralelisme, konsistensi state, dan fault tolerance. Implementasi ini memungkinkan Klomang Core untuk memproses transaksi dengan efisiensi tinggi sambil mempertahankan guarantees keamanan dan konsistensi yang diperlukan untuk blockchain yang handal.