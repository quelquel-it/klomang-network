# Manajemen State Klomang Core

## Pendahuluan

Klomang Core mengimplementasikan sistem manajemen state yang canggih dan efisien, menggabungkan model UTXO (Unspent Transaction Output) dengan Verkle tree untuk commitment state yang kriptografis. Sistem ini dirancang untuk mendukung transaksi konvensional, smart contract, dan operasi blockchain yang scalable. Dokumen ini menyajikan analisis mendalam tentang komponen-komponen manajemen state yang digunakan dalam Klomang Core.

## 1. Arsitektur State Blockchain

### 1.1 BlockchainState

Struktur utama yang mengelola state konsensus dan finalitas blockchain.

```rust
pub struct BlockchainState {
    pub finalizing_block: Option<Hash>,
    pub virtual_score: u64,
    pub pruned: Vec<Hash>,
    pub utxo_set: UtxoSet,
    pub prune_markers: HashMap<OutPoint, PruneMarker>,
}
```

#### Komponen Utama
- **Finalizing Block**: Blok yang menentukan urutan final dalam DAG
- **Virtual Score**: Skor DAG virtual untuk konsensus
- **Pruned Blocks**: Blok yang sudah tidak diperlukan (pruning)
- **UTXO Set**: Set output transaksi yang belum terpakai
- **Prune Markers**: Metadata untuk pruning UTXO entries

#### Mekanisme Pruning
```rust
pub struct PruneMarker {
    pub epoch: u64,
    pub timestamp: u64,
}
```
- **Epoch-based Pruning**: Pruning berdasarkan epoch untuk efisiensi storage
- **Timestamp Tracking**: Pelacakan waktu untuk pruning yang aman
- **Incremental Cleanup**: Pembersihan bertahap untuk menghindari disruption

## 2. Model Transaksi

### 2.1 Struktur Transaksi

Klomang Core menggunakan model transaksi yang extensible dengan dukungan smart contract.

```rust
pub struct Transaction {
    pub id: Hash,
    pub inputs: Vec<TxInput>,
    pub outputs: Vec<TxOutput>,
    pub execution_payload: Vec<u8>,
    pub contract_address: Option<Address>,
    pub gas_limit: u64,
    pub max_fee_per_gas: u128,
    pub chain_id: u32,
    pub locktime: u32,
}
```

#### Komponen Transaksi
- **ID Transaksi**: Hash deterministik dari komponen transaksi
- **Inputs**: Referensi ke output transaksi sebelumnya
- **Outputs**: Output baru yang dibuat
- **Execution Payload**: Bytecode untuk smart contract execution
- **Contract Address**: Alamat kontrak untuk interaksi
- **Gas Parameters**: Limit gas dan harga maksimal per gas
- **Chain ID**: Identifier chain untuk mencegah cross-chain replay
- **Locktime**: Waktu minimum untuk inclusion dalam blok

### 2.2 Input dan Output Transaksi

#### TxInput
```rust
pub struct TxInput {
    pub prev_tx: Hash,
    pub index: u32,
    pub signature: Vec<u8>,
    pub pubkey: Vec<u8>,
    pub sighash_type: SigHashType,
}
```
- **OutPoint Reference**: Referensi ke (tx_id, output_index)
- **Signature**: Tanda tangan Schnorr untuk otorisasi
- **Pubkey**: Kunci publik untuk verifikasi
- **SigHash Type**: Tipe sighash untuk fleksibilitas signing

#### TxOutput
```rust
pub struct TxOutput {
    pub value: u64,
    pub pubkey_hash: Hash,
}
```
- **Value**: Jumlah Nano-SLUG dalam output
- **Pubkey Hash**: Hash dari kunci publik penerima

### 2.3 SigHash Types

Klomang Core mendukung multiple signature hash types untuk fleksibilitas transaksi:

- **SigHashType::All**: Semua input dan output harus tetap sama
- **SigHashType::None**: Hanya input yang ditandatangani, output dapat berubah
- **SigHashType::Single**: Hanya input dan output dengan indeks yang sama

### 2.4 Coinbase Transactions

- **Input Kosong**: Transaksi coinbase tidak memiliki input
- **Reward Distribution**: Membuat output untuk miner dan full node rewards
- **Validation Khusus**: Validasi berbeda untuk transaksi coinbase

## 3. Sistem UTXO

### 3.1 UtxoSet

Struktur utama untuk tracking output transaksi yang belum terpakai.

```rust
pub struct UtxoSet {
    pub utxos: HashMap<OutPoint, TxOutput>,
}
```

#### Operasi Utama
- **Validation**: Verifikasi input transaksi terhadap UTXO set
- **Update**: Atomic update melalui changeset
- **Pruning**: Penghapusan UTXO yang sudah terpakai

### 3.2 UtxoChangeSet

Struktur untuk atomic transaction updates.

```rust
pub struct UtxoChangeSet {
    pub spent: Vec<OutPoint>,
    pub created: Vec<(OutPoint, TxOutput)>,
}
```
- **Spent**: Output yang dikonsumsi dalam transaksi
- **Created**: Output baru yang dibuat
- **Atomicity**: Semua perubahan diterapkan atau tidak sama sekali

### 3.3 Kebijakan Anti-Deflasi

UTXO set menerapkan kebijakan ketat untuk mencegah pembakaran koin:

```rust
const ZERO_ADDRESS: [u8; 32] = [0u8; 32];
```
- **Burn Address Rejection**: Semua output ke alamat nol ditolak
- **Economic Enforcement**: Memastikan 100% Nano-SLUG tetap dalam sirkulasi
- **Validation Layer**: Pengecekan pada level transaksi dan blok

## 4. Sistem Storage

### 4.1 Storage Interface

Abstraksi storage yang generic untuk fleksibilitas implementasi.

```rust
pub trait Storage {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>>;
    fn put(&mut self, key: Vec<u8>, value: Vec<u8>);
    fn delete(&mut self, key: &[u8]);
}
```

#### Operasi Dasar
- **Get**: Retrieve value berdasarkan key
- **Put**: Store key-value pair
- **Delete**: Remove key-value pair

### 4.2 MemoryStorage Implementation

Implementasi in-memory untuk development dan testing.

```rust
pub struct MemoryStorage {
    pub map: HashMap<Vec<u8>, Vec<u8>>,
}
```
- **In-Memory**: Semua data disimpan dalam RAM
- **Thread-Unsafe**: Tidak thread-safe untuk concurrent access
- **Testing Purpose**: Digunakan untuk unit tests dan development

## 5. Verkle Tree Integration

### 5.1 V_Trie Wrapper

Wrapper untuk Verkle tree dengan integrasi khusus Klomang Core.

```rust
pub struct VerkleTree<S: Storage> {
    // Implementation details...
}
```

#### Fitur Khusus
- **Gas Fee Witness**: Tracking distribusi fee gas 80/20
- **Total Supply Tracking**: Monitoring total supply dalam tree
- **Incremental Updates**: Update efisien tanpa rebuild penuh

### 5.2 Gas Fee Witness

Struktur untuk validasi distribusi fee gas.

```rust
pub struct GasFeeWitness {
    pub total_gas_fee: u128,
    pub miner_share: u128,
    pub fullnode_share: u128,
}
```
- **Total Gas Fee**: Total fee gas yang dikumpulkan
- **Miner Share**: 80% untuk penambang
- **Fullnode Share**: 20% untuk operator full node

### 5.3 Verkle Proofs

Proofs untuk verifikasi state tanpa mengakses full tree.

```rust
pub struct VerkleProof {
    pub proof_type: ProofType,
    pub path: Vec<u8>,
    pub siblings: Vec<[u8; 32]>,
    pub leaf_value: Option<Vec<u8>>,
    pub root: [u8; 32>,
    pub opening_proofs: Vec<OpeningProof>,
}
```
- **Membership Proofs**: Bukti keberadaan key-value pair
- **Non-Membership Proofs**: Bukti ketidakberadaan key
- **Opening Proofs**: IPA proofs untuk polynomial evaluation

## 6. Access Set untuk Conflict Detection

### 6.1 AccessSet Structure

Struktur untuk tracking akses state oleh transaksi.

```rust
pub struct AccessSet {
    pub read_set: HashSet<[u8; 32]>,
    pub write_set: HashSet<[u8; 32]>,
}
```

#### Mekanisme Conflict Detection
```rust
pub fn has_conflict(&self, other: &AccessSet) -> bool {
    // Conflict if any write set overlaps with any read or write set of other
    !self.write_set.is_disjoint(&other.read_set) ||
    !self.write_set.is_disjoint(&other.write_set) ||
    !other.write_set.is_disjoint(&self.read_set)
}
```
- **Read-Write Conflicts**: Write ke key yang sedang dibaca
- **Write-Write Conflicts**: Multiple writes ke key yang sama
- **Cross-Transaction Conflicts**: Deteksi konflik antar transaksi

### 6.2 Access Set Merging

```rust
pub fn merge(&mut self, other: &AccessSet) {
    self.read_set.extend(&other.read_set);
    self.write_set.extend(&other.write_set);
}
```
- **Union Operation**: Menggabungkan read dan write sets
- **Batch Processing**: Untuk multiple transaksi dalam blok

## 7. Mekanisme Validasi State

### 7.1 Transaction Validation

- **Input Validation**: Verifikasi ownership melalui signature
- **UTXO Validation**: Pastikan input tersedia dan belum terpakai
- **Balance Validation**: Input >= Output + Fee
- **Script Validation**: Untuk smart contract transactions

### 7.2 State Transition Validation

- **Verkle Proof Verification**: Validasi state changes melalui proofs
- **Gas Accounting**: Tracking gas usage dan fee collection
- **Access Control**: Validasi permissions untuk state modifications

### 7.3 Block State Updates

- **Atomic Updates**: Semua perubahan state dalam blok diterapkan atomically
- **Rollback Capability**: Mekanisme untuk revert state jika diperlukan
- **Finality Tracking**: Update finality status berdasarkan konsensus

## 8. Optimisasi dan Performa

### 8.1 Incremental State Updates

- **Lazy Evaluation**: State changes dievaluasi on-demand
- **Caching**: Commitment caching untuk Verkle tree nodes
- **Batch Operations**: Pengelompokan updates untuk efisiensi

### 8.2 Storage Optimization

- **Pruning**: Penghapusan state yang tidak diperlukan
- **Compression**: Kompresi data untuk storage efficiency
- **Indexing**: Indexing untuk fast lookups

### 8.3 Memory Management

- **Garbage Collection**: Cleanup unused state entries
- **Memory Pool**: Pool untuk temporary state objects
- **Reference Counting**: Tracking reference untuk efficient cleanup

## Kesimpulan

Sistem manajemen state Klomang Core merupakan implementasi yang komprehensif dan efisien, menggabungkan model UTXO dengan Verkle tree untuk state commitment yang kriptografis. Melalui struktur transaksi yang extensible, validasi anti-deflasi yang ketat, dan mekanisme conflict detection yang canggih, sistem ini memastikan integritas state blockchain sambil mendukung skalabilitas dan fleksibilitas untuk smart contract. Implementasi ini memungkinkan Klomang Core untuk menangani transaksi konvensional dan kontrak cerdas dengan efisiensi tinggi dan keamanan kriptografis yang terjamin.