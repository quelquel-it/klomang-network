# Kriptografi Klomang Core

## Pendahuluan

Klomang Core mengimplementasikan suite kriptografi yang komprehensif dan canggih, menggabungkan algoritma hash modern, skema tanda tangan digital, dan struktur data Verkle tree untuk memastikan keamanan, efisiensi, dan skalabilitas. Sistem kriptografi ini dirancang untuk mendukung operasi blockchain yang aman, termasuk verifikasi transaksi, commitment state, dan proof-of-work. Dokumen ini menyajikan analisis mendalam tentang komponen-komponen kriptografi yang digunakan dalam Klomang Core.

## 1. Fungsi Hash

### 1.1 Algoritma Blake3

Klomang Core menggunakan algoritma hash Blake3 sebagai fungsi hash utama untuk semua operasi kriptografi.

#### Karakteristik Utama
- **Kecepatan**: Algoritma hash tercepat yang tersedia, dengan throughput > 1 GB/detik pada hardware modern
- **Keamanan**: Berbasis konstruksi sponge yang terbukti aman secara kriptografis
- **Output**: 256-bit (32 byte) hash output
- **Penggunaan**: Hash blok, hash transaksi, hash state, dan hash untuk proof-of-work

#### Implementasi Hash
```rust
#[derive(Clone, Eq, PartialEq, Hash, PartialOrd, Ord, Debug)]
pub struct Hash([u8; 32]);
```
- **Serialisasi**: Mendukung format hex untuk display dan debugging
- **Komparasi**: Mengimplementasikan trait Eq, PartialEq, Hash, PartialOrd, Ord untuk penggunaan dalam struktur data
- **Konversi**: Method untuk konversi ke/dari byte array dan string hex

#### Keunggulan Blake3
- **Paralelisme**: Dirancang untuk memanfaatkan SIMD dan multi-threading
- **Ekstensibilitas**: Mendukung arbitrary-length output melalui extendable output function (XOF)
- **Domain Separation**: Menggunakan tagged hashing untuk mencegah collision attacks

## 2. Skema Tanda Tangan Digital

### 2.1 Schnorr Signatures

Klomang Core menggunakan Schnorr signatures berdasarkan kurva elliptik secp256k1 melalui library k256.

#### Parameter Kriptografi
- **Kurva**: secp256k1 (standard Bitcoin/ECDSA)
- **Ukuran Kunci**: 256-bit private key, 264-bit compressed public key
- **Ukuran Tanda Tangan**: 64 byte (r, s)
- **Hash Function**: Blake3 untuk message hashing

#### Implementasi KeyPairWrapper
```rust
pub struct KeyPairWrapper {
    signing_key: SigningKey,
}
```
- **Key Generation**: Random generation menggunakan OsRng
- **Deterministic Keys**: Derivasi deterministik dari seed menggunakan Blake3
- **Fallback Mechanism**: Retry hingga 1024 kali untuk menghindari scalar nol

#### Tagged Hashing untuk Domain Separation
```rust
pub fn tagged_hash(tag: &str, data: &[u8]) -> [u8; 32]
```
- **Tag**: "KLOMANG_TX_V1" untuk transaksi
- **Konstruksi**: H(tag) || H(tag) || data (BIP340-style)
- **Tujuan**: Mencegah cross-protocol attacks dan replay attacks

#### SigHash Types
Klomang Core mendukung multiple sighash types untuk fleksibilitas transaksi:
- **SigHashType::All**: Menandatangani semua input dan output
- **SigHashType::None**: Menandatangani input saja, output dapat dimodifikasi
- **SigHashType::Single**: Menandatangani input dan output dengan indeks yang sama

#### Serialisasi Transaksi untuk Signing
```rust
pub fn serialize_tx_for_sighash(tx: &Transaction, input_index: usize, sighash: SigHashType) -> Vec<u8>
```
- **Chain ID**: Termasuk dalam serialisasi untuk mencegah cross-chain replay
- **Input Handling**: Mengganti input yang ditandatangani dengan pubkey untuk SIGHASH_ALL
- **Output Inclusion**: Berdasarkan sighash type

## 3. Verkle Tree

### 3.1 Arsitektur 256-ary Verkle Tree

Verkle tree adalah struktur data kriptografi yang menggabungkan keunggulan Merkle tree dengan polynomial commitments untuk proof yang lebih efisien.

#### Parameter Utama
- **Radix**: 256 (256-ary tree)
- **Key Size**: 32 byte (256-bit)
- **Leaf Values**: Variable-length byte arrays
- **Commitment Scheme**: Inner Product Argument (IPA) dengan Bandersnatch curve

#### Polynomial Commitments

##### Inner Product Argument (IPA)
Klomang Core menggunakan IPA sebagai dasar untuk polynomial commitments:

- **Kurva**: Bandersnatch (Edwards curve pada BLS12-381)
- **Field**: Prime field dari Bandersnatch curve
- **Generator Points**: Deterministik generation menggunakan hash-to-curve
- **Commitment Size**: 32 byte (compressed point)

##### Implementasi PolynomialCommitment
```rust
pub struct PolynomialCommitment {
    pub generators: Vec<EdwardsAffine>,
    pub random_point: EdwardsAffine,
}
```
- **Generator Generation**: Deterministik menggunakan Blake3 hash-to-curve
- **Commitment Creation**: Variable-base multi-scalar multiplication (MSM)
- **Opening Proofs**: IPA proofs untuk verifikasi evaluasi polynomial

#### Struktur Node Verkle Tree

##### Cached Node untuk Incremental Updates
```rust
struct CachedNode {
    commitment: Option<Commitment>,
    dirty: bool,
}
```
- **Commitment Caching**: Menyimpan commitment di setiap node untuk efisiensi
- **Dirty Flag**: Menandai node yang perlu update
- **Incremental Updates**: Menghindari recomputation penuh tree

##### Verkle Tree Implementation
```rust
pub struct VerkleTree<S: Storage> {
    root: Option<VerkleNode>,
    storage: S,
    commitment_scheme: PolynomialCommitment,
}
```
- **Storage Backend**: Generic storage interface untuk persistensi
- **Root Commitment**: Commitment dari root polynomial
- **Batch Operations**: Mendukung update batch untuk efisiensi

#### Operasi Utama

##### Insertion/Update
- **Path Resolution**: 256-ary path resolution dari key
- **Polynomial Construction**: Interpolation dari key-value pairs
- **Commitment Update**: Incremental commitment update
- **Proof Generation**: Opening proofs untuk verifikasi

##### Membership Proofs
```rust
pub struct VerkleProof {
    pub proof_type: ProofType,
    pub path: Vec<u8>,
    pub siblings: Vec<[u8; 32]>,
    pub leaf_value: Option<Vec<u8>>,
    pub root: [u8; 32],
    pub opening_proofs: Vec<OpeningProof>,
}
```
- **Path**: Jalur dari root ke leaf
- **Siblings**: Commitment siblings untuk path verification
- **Opening Proofs**: IPA proofs untuk setiap level
- **Verification**: Verifikasi tanpa mengakses full tree

##### Non-Membership Proofs
- **Proof of Absence**: Menunjukkan key tidak ada dalam tree
- **Boundary Proofs**: Menggunakan polynomial evaluation untuk proof absence

#### Optimisasi dan Efisiensi

##### Incremental Updates
- **Dirty Tracking**: Hanya update node yang terpengaruh
- **Commitment Propagation**: Bottom-up commitment recalculation
- **Batch Processing**: Mengelompokkan multiple updates

##### Proof Size Optimization
- **IPA Efficiency**: Proof size O(log n) dibandingkan O(n) Merkle proofs
- **Aggregation**: Multiple proofs dapat diagregasi
- **Verification Cost**: Konstanta time verification

##### Storage Efficiency
- **Compact Representation**: Commitment-based storage
- **Lazy Loading**: Load node on-demand
- **Garbage Collection**: Cleanup unused nodes

## 4. Integrasi Sistem Kriptografi

### 4.1 Dalam Konsensus
- **Block Hashing**: Blake3 untuk block headers
- **Transaction Verification**: Schnorr signature verification
- **State Commitment**: Verkle tree root sebagai state root
- **Proof-of-Work**: Blake3-based mining

### 4.2 Dalam State Management
- **UTXO Tracking**: Verkle tree untuk UTXO set
- **Account State**: Verkle tree untuk account balances
- **Contract State**: Verkle tree untuk contract storage
- **Historical Proofs**: Verkle proofs untuk state verification

### 4.3 Dalam Virtual Machine
- **Gas Accounting**: Blake3 untuk gas measurement
- **Contract Verification**: Schnorr signatures untuk contract deployment
- **Execution Proofs**: Verkle proofs untuk execution verification

## 5. Keamanan dan Analisis Kriptografi

### 5.1 Asumsi Keamanan
- **Blake3**: Collision resistance, preimage resistance, second preimage resistance
- **Schnorr**: EUF-CMA security dalam random oracle model
- **IPA**: Knowledge soundness, zero-knowledge properties
- **Verkle Tree**: Merkle tree security dengan polynomial commitment guarantees

### 5.2 Perlindungan terhadap Serangan
- **Collision Attacks**: Domain separation melalui tagged hashing
- **Replay Attacks**: Chain ID inclusion dalam transaction signing
- **Forgery Attacks**: Schnorr signature security
- **State Manipulation**: Verkle tree cryptographic integrity

### 5.3 Performa dan Skalabilitas
- **Hash Performance**: >1 GB/s throughput
- **Signature Verification**: ~100μs per verification
- **Verkle Proofs**: O(log n) proof size dan verification time
- **Incremental Updates**: Near-constant time untuk small updates

## Kesimpulan

Suite kriptografi Klomang Core merupakan implementasi state-of-the-art yang menggabungkan algoritma modern dengan optimisasi praktis. Melalui penggunaan Blake3 untuk hashing, Schnorr signatures untuk authentication, dan Verkle trees untuk state commitment, sistem ini mencapai keseimbangan optimal antara keamanan kriptografi, efisiensi komputasi, dan skalabilitas. Implementasi ini memastikan bahwa Klomang Core dapat beroperasi secara aman dan efisien dalam skala global, sambil mempertahankan integritas dan verifiabilitas semua operasi blockchain.