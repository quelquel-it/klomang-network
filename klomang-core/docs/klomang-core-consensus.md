# Sistem Konsensus Klomang Core

## Pendahuluan

Klomang Core mengimplementasikan sistem konsensus berbasis Directed Acyclic Graph (DAG) yang inovatif, menggabungkan algoritma GHOSTDAG dengan mekanisme Proof-of-Work (PoW) adaptif. Sistem ini dirancang untuk mencapai finalitas probabilistik tinggi, ketahanan terhadap serangan, dan skalabilitas yang optimal dalam kondisi jaringan yang bervariasi. Dokumen ini menyajikan analisis mendalam tentang komponen-komponen konsensus, algoritma, dan mekanisme validasi yang mendasari Klomang Core.

## 1. Arsitektur Konsensus

### 1.1 Algoritma GHOSTDAG

GHOSTDAG (Greedy Heaviest-Observed Sub-Tree DAG) adalah algoritma konsensus utama yang digunakan dalam Klomang Core. Algoritma ini memungkinkan pemrosesan paralel blok-blok yang bersaing, mengurangi pemborosan komputasi yang terjadi dalam blockchain tradisional dengan fork yang tidak produktif.

#### Parameter K (Ghostdag Parameter)
- **Rentang**: 1 hingga 64
- **Fungsi**: Menentukan kedalaman pencarian untuk memilih "selected parent" dan membangun blue set
- **Adaptivitas**: Parameter k disesuaikan secara dinamis berdasarkan kondisi jaringan:
  - **Kenaikan k**: Ketika beban jaringan > 80%, k meningkat untuk mengurangi keuntungan penambangan egois
  - **Penurunan k**: Ketika beban jaringan < 20%, k menurun untuk meningkatkan performa
  - **Interval penyesuaian**: Setiap 1 jam

#### Blue Score
Blue score adalah metrik utama yang menentukan bobot dan urutan blok dalam DAG:
- **Inisialisasi**: Blok genesis memiliki blue score 0
- **Propagasi**: Blue score dihitung sebagai blue score selected parent + 1
- **Finalitas**: Blok dengan blue score yang cukup tinggi (> finality depth) dianggap final

### 1.2 Finality Depth

- **Nilai**: 100 blok
- **Implikasi**: Blok yang mencapai kedalaman 100 blok tidak dapat direorganisasi
- **Kepastian**: Memberikan finalitas probabilistik yang tinggi untuk transaksi

## 2. Mekanisme Validasi Blok

### 2.1 Validasi Konektivitas DAG
Setiap blok baru harus memenuhi kriteria konektivitas:
- Semua parent blok harus ada dalam DAG saat ini
- Blok tidak boleh mengandung referensi parent yang tidak valid

### 2.2 Validasi Timestamp
- **Toleransi masa depan**: Maksimal 2 jam ke depan dari waktu saat ini
- **Toleransi masa lalu**: Maksimal 24 jam ke belakang dari waktu saat ini
- **Tujuan**: Mencegah serangan replay dan memastikan urutan temporal yang konsisten

### 2.3 Validasi Kesulitan (Difficulty)
- **Kebijakan**: Kesulitan tidak boleh nol
- **Penyesuaian**: Disesuaikan berdasarkan algoritma DAA (Difficulty Adjustment Algorithm)
- **Target**: Mempertahankan waktu blok rata-rata sesuai dengan parameter jaringan

### 2.4 Validasi Proof-of-Work (PoW)
- **Algoritma**: Menggunakan fungsi hash yang tahan terhadap ASIC
- **Verifikasi**: Hash blok harus memenuhi target kesulitan
- **Komponen hash**: Timestamp, kesulitan, parents, blue score, nonce, dan transaction root

### 2.5 Validasi Transaksi
- **Integritas**: Semua transaksi dalam blok harus valid
- **Konsistensi**: Input dan output transaksi harus seimbang
- **Duplikasi**: Tidak ada transaksi duplikat dalam blok

### 2.6 Validasi Integritas State (Verkle Proof)
- **Teknologi**: Menggunakan Verkle Tree untuk state commitment
- **Verifikasi**: Proof Verkle memastikan state transisi yang valid
- **Efisiensi**: Mengurangi ukuran proof dibandingkan Merkle Tree tradisional

## 3. Sistem Emisi dan Reward

### 3.1 Jadwal Emisi
- **Reward awal**: 100 SLUG per blok
- **Interval halving**: Setiap 100.000 blok (berdasarkan DAA score)
- **Reward minimum**: 1 Nano-SLUG
- **Total pasokan maksimal**: 600.000.000 SLUG (600 juta)

### 3.2 Distribusi Reward
- **Rasio tetap**: 80% untuk penambang, 20% untuk full node operators
- **Kondisi khusus**: Jika tidak ada full node aktif, 100% reward diberikan ke penambang
- **Fee collection**: Semua transaction fee dan gas fee masuk ke reward pool

### 3.3 Kebijakan Anti-Inflasi
- **Tidak ada pembakaran**: Semua fee dikumpulkan ke reward pool
- **Supply cap enforcement**: Emisi dihentikan ketika mencapai batas maksimal
- **Deterministik**: Perhitungan reward bersifat deterministik dan dapat diverifikasi

## 4. Mekanisme Ordering dan Finalitas

### 4.1 Ordering Blok
- **Kriteria utama**: Blue score sebagai metrik utama
- **Kriteria sekunder**: Hash blok untuk tie-breaking
- **Deterministik**: Ordering selalu konsisten di seluruh jaringan

### 4.2 Finalitas Probabilistik
- **Depth-based**: Finalitas dicapai pada kedalaman tertentu
- **Irreversibilitas**: Blok final tidak dapat direorganisasi
- **Konfirmasi**: Transaksi dianggap final setelah beberapa konfirmasi

## 5. Adaptivitas dan Ketahanan

### 5.1 Penyesuaian Parameter Dinamis
- **K parameter**: Disesuaikan berdasarkan kondisi jaringan
- **Responsivitas**: Sistem dapat beradaptasi dengan perubahan beban jaringan
- **Stabilitas**: Mencegah osilasi parameter yang berlebihan

### 5.2 Ketahanan terhadap Serangan
- **Selfish mining**: Dikurangi melalui penyesuaian k yang adaptif
- **Eclipse attack**: Dicegah melalui validasi konektivitas DAG
- **Long-range attack**: Dicegah melalui finality depth dan state commitment

### 5.3 Skalabilitas
- **Paralelisme**: DAG memungkinkan pemrosesan paralel blok
- **Throughput**: Meningkat seiring dengan ukuran jaringan
- **Efisiensi**: Mengurangi orphaned block dibandingkan blockchain linear

## 6. Integrasi dengan Komponen Lain

### 6.1 State Management
- **Verkle Tree**: Untuk commitment state yang efisien
- **UTXO Model**: Untuk tracking kepemilikan koin
- **Storage Layer**: Persistent storage dengan proof integrity

### 6.2 Virtual Machine
- **Gas metering**: Pengukuran konsumsi sumber daya
- **Execution environment**: Deterministik dan isolated
- **Contract deployment**: Mendukung smart contract

### 6.3 Networking
- **Gossip protocol**: Penyebaran blok dan transaksi
- **Peer discovery**: Mekanisme penemuan node
- **Synchronization**: Fast sync untuk node baru

## Kesimpulan

Sistem konsensus Klomang Core merupakan implementasi canggih dari teknologi DAG yang menggabungkan keunggulan GHOSTDAG dengan mekanisme adaptif. Melalui parameter k yang dapat disesuaikan, finality depth yang ketat, dan validasi multi-layer, sistem ini mencapai keseimbangan optimal antara keamanan, skalabilitas, dan efisiensi. Implementasi ini memastikan bahwa Klomang Core dapat beroperasi secara handal dalam berbagai kondisi jaringan, sambil mempertahankan integritas ekonomi dan konsistensi state.