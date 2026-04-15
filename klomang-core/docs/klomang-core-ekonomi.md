# Hukum Ekonomi Klomang Core

## Pendahuluan
Klomang Core adalah sistem blockchain yang mengimplementasikan model ekonomi yang ketat dan tidak dapat diubah. Hukum ekonomi ini dirancang untuk memastikan kelanggengan, keadilan distribusi, dan pencegahan deflasi. Semua parameter ekonomi dikunci secara permanen dalam kode dan tidak dapat diubah tanpa upgrade jaringan yang terkoordinasi.

## 1. Batas Pasokan (Supply Cap)
- **Batas maksimal pasokan**: 600 juta koin SLUG (600.000.000 SLUG)
- **Dalam unit terkecil (Nano-SLUG)**: 600.000.000.000.000.000 unit
- **Kebijakan**: Pasokan total tidak boleh melebihi batas ini di bawah kondisi apa pun. Setiap upaya untuk melebihi batas akan menghasilkan reward nol.

## 2. Sistem Emisi Blok
- **Reward blok awal**: 100 SLUG per blok
- **Interval halving**: Setiap 100.000 blok (berdasarkan DAA score)
- **Reward minimum**: 1 unit Nano-SLUG
- **Formula halving**: Reward berkurang setengah setiap interval, dengan batas minimum 1 unit
- **Total emisi**: Diperkirakan mencapai batas maksimal setelah serangkaian halving yang ditentukan

## 3. Distribusi Reward
- **Pembagian tetap**: 80% untuk penambang (miner), 20% untuk simpul penuh (full node)
- **Kebijakan**: Pembagian ini berlaku tanpa syarat selama fase emisi dan pasca-emisi
- **Kasus khusus**: Jika tidak ada simpul penuh yang aktif, 100% reward diberikan kepada penambang (bukan dibakar)
- **Perhitungan**: Reward dihitung berdasarkan total pool (subsidi blok + fee transaksi), kemudian dibagi sesuai rasio

## 4. Kebijakan Anti-Deflasi
- **Larangan pembakaran**: Tidak ada koin yang boleh dibakar atau dikirim ke alamat nol ([0u8; 32])
- **Pengumpulan fee**: Semua fee transaksi dan gas fee masuk ke pool reward
- **Subsidi blok**: Semua subsisi blok masuk ke pool reward
- **Transaksi coinbase**: Harus memiliki alamat penerima yang valid dan bukan nol

## 5. Sistem Fee Gas
- **Pengumpulan gas fee**: 100% gas fee dikumpulkan ke pool reward
- **Formula**: total_gas_fee = gas_used * max_fee_per_gas
- **Kebijakan**: Tidak ada gas fee yang dibakar; semua mendukung jaringan
- **Minimum gas price**: 1 unit Nano-SLUG untuk mencegah spam

## 6. Mekanisme Validasi
- **Validasi reward penambang**: Memastikan pembagian reward sesuai dengan rasio 80/20
- **Validasi simpul penuh**: Hanya simpul yang terdaftar dan tersedia datanya yang eligible untuk reward 20%
- **Validasi transaksi**: Fee dihitung sebagai selisih input - output, dengan semua fee masuk pool reward

## Kesimpulan
Hukum ekonomi Klomang Core dirancang untuk menciptakan ekosistem yang stabil dan berkelanjutan. Dengan batas pasokan yang ketat, distribusi reward yang adil, dan kebijakan anti-deflasi, sistem ini memastikan nilai koin SLUG terjaga seiring waktu. Semua parameter dikunci secara permanen untuk mencegah manipulasi dan memastikan kepercayaan jaringan.