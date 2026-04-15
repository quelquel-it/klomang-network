# Virtual Machine Klomang Core

## Pendahuluan

Klomang Core mengimplementasikan Virtual Machine (VM) yang powerful dan efisien untuk eksekusi smart contract menggunakan WebAssembly (WASM). VM ini terintegrasi dengan sistem gas metering yang komprehensif, host functions untuk interaksi state, dan mekanisme eksekusi yang aman. Sistem ini dirancang untuk mendukung kontrak cerdas yang kompleks sambil mempertahankan keamanan, determinisme, dan efisiensi sumber daya. Dokumen ini menyajikan analisis mendalam tentang komponen-komponen Virtual Machine yang digunakan dalam Klomang Core.

## 1. Arsitektur VM

### 1.1 VMExecutor

Komponen utama yang mengelola eksekusi WASM contracts.

```rust
pub struct VMExecutor;
```

#### Teknologi Dasar
- **WebAssembly (WASM)**: Format bytecode yang efisien dan portable
- **Wasmer Runtime**: High-performance WASM runtime
- **Cranelift Compiler**: JIT compiler untuk optimisasi performa
- **Metering Middleware**: Gas metering untuk resource accounting

### 1.2 Host Environment

Environment untuk interaksi antara WASM dan host system.

```rust
struct HostEnv<S: Storage + Clone + Send + Sync> {
    state_ptr: *mut StateManager<S>,
    gas: RefCell<GasMeter>,
    instance_backref: RefCell<Option<Instance>>,
}
```

#### Komponen Host
- **State Pointer**: Pointer aman ke StateManager
- **Gas Meter**: Tracking konsumsi gas
- **Instance Reference**: Back-reference ke WASM instance

## 2. Sistem Gas Metering

### 2.1 GasMeter Structure

Struktur utama untuk accounting gas consumption.

```rust
pub struct GasMeter {
    pub initial: i128,
    pub remaining: i128,
    pub consumed_wasm: u64,
    pub consumed_host: u64,
    pub refund: u64,
}
```

#### Gas Accounting
- **Initial**: Gas limit awal untuk eksekusi
- **Remaining**: Gas yang tersisa
- **Consumed WASM**: Gas yang dikonsumsi oleh opcodes WASM
- **Consumed Host**: Gas yang dikonsumsi oleh host functions
- **Refund**: Gas yang dapat direfund (misalnya self-destruct)

### 2.2 Gas Costs

Konstanta gas costs untuk berbagai operasi.

```rust
pub const INTRINSIC_GAS: GasCost = 21_000;
pub const STATE_READ_COST: GasCost = 2_100;
pub const STATE_WRITE_NEW_COST: GasCost = 20_000;
pub const STATE_WRITE_UPDATE_COST: GasCost = 5_000;
pub const SELF_DESTRUCT_REFUND: GasCost = 24_000;
```

#### Kategori Gas Costs
- **Intrinsic Gas**: Biaya dasar untuk transaction processing
- **State Operations**: Biaya untuk read/write state
- **Payload Gas**: Biaya berdasarkan ukuran bytecode
- **Refunds**: Pengembalian gas untuk operasi efisien

### 2.3 Gas Charging Methods

Method untuk charging gas pada berbagai operasi.

```rust
pub fn charge_intrinsic(&mut self) -> Result<(), VMError>
pub fn charge_payload(&mut self, payload: &[u8]) -> Result<(), VMError>
pub fn charge_state_read(&mut self) -> Result<(), VMError>
pub fn charge_state_write(&mut self, is_new: bool) -> Result<(), VMError>
```

#### Payload Charging
```rust
pub fn charge_payload(&mut self, payload: &[u8]) -> Result<(), VMError> {
    let mut total_cost: u64 = 0;
    for byte in payload {
        total_cost = total_cost.saturating_add(if *byte == 0 { 4 } else { 16 });
    }
    self.consume_host(total_cost)
}
```
- **Zero Bytes**: 4 gas per byte (lebih murah karena compressible)
- **Non-Zero Bytes**: 16 gas per byte
- **Saturating Add**: Mencegah overflow

## 3. Mekanisme Eksekusi

### 3.1 Execute Method

Method utama untuk eksekusi WASM contract.

```rust
pub fn execute<S>(
    wasm_bytes: &[u8],
    state_manager: &mut StateManager<S>,
    _sender: Address,
    gas_limit: u64,
) -> Result<(Vec<u8>, GasMeter), VMError>
```

#### Parameter Eksekusi
- **wasm_bytes**: Bytecode WASM yang akan dieksekusi
- **state_manager**: Reference ke StateManager untuk state access
- **sender**: Address pengirim transaksi
- **gas_limit**: Batas maksimal gas untuk eksekusi

#### Return Values
- **Result**: Output dari contract execution atau error
- **GasMeter**: Final state gas meter untuk fee calculation

### 3.2 WASM Metering Integration

Integrasi metering langsung ke WASM runtime.

```rust
fn wasm_meter_cost(operator: &Operator) -> u64 {
    GasMeter::opcode_cost(operator)
}
```

#### Opcode Cost Calculation
```rust
pub fn opcode_cost(op: &Operator) -> u64
```
- **Arithmetic Operations**: Cost berdasarkan kompleksitas
- **Memory Operations**: Cost berdasarkan ukuran memory access
- **Control Flow**: Cost untuk branches dan calls

### 3.3 Metering Points Management

Management points metering dalam WASM instance.

```rust
fn charge_metering_from_host(
    store: &mut impl wasmer::AsStoreMut,
    instance: &Instance,
    cost: u64,
) -> Result<(), VMError>
```

#### Metering Logic
- **Remaining Points Check**: Verifikasi gas tersedia
- **Out-of-Gas Detection**: Error jika gas habis
- **Points Deduction**: Kurangi points sesuai cost

## 4. Host Functions

### 4.1 State Access Functions

Host functions untuk interaksi dengan blockchain state.

#### State Read
```rust
fn host_state_read(env: FunctionEnvMut<HostEnv<S>>, key_ptr: u32, key_len: u32, value_ptr: u32, value_len_ptr: u32) -> u64
```
- **Parameters**: Pointer ke key, buffer untuk value
- **Gas Charging**: Charge STATE_READ_COST
- **Return**: Length of value atau error code

#### State Write
```rust
fn host_state_write(env: FunctionEnvMut<HostEnv<S>>, key_ptr: u32, key_len: u32, value_ptr: u32, value_len: u32) -> u64
```
- **Parameters**: Key dan value untuk write
- **Gas Charging**: Charge berdasarkan operasi (new/update)
- **Atomicity**: Write langsung ke state

### 4.2 Cryptographic Functions

Host functions untuk operasi kriptografi.

#### Hash Function
```rust
fn host_sha256(env: FunctionEnvMut<HostEnv<S>>, data_ptr: u32, data_len: u32, hash_ptr: u32) -> u64
```
- **Input**: Data untuk di-hash
- **Output**: SHA256 hash result
- **Gas Cost**: Fixed cost untuk hash operation

#### Signature Verification
```rust
fn host_verify_sig(env: FunctionEnvMut<HostEnv<S>>, 
                   pubkey_ptr: u32, pubkey_len: u32,
                   msg_ptr: u32, msg_len: u32,
                   sig_ptr: u32, sig_len: u32) -> u64
```
- **Parameters**: Public key, message, signature
- **Verification**: Schnorr signature verification
- **Return**: 1 untuk valid, 0 untuk invalid

### 4.3 Utility Functions

Functions utilitas untuk contract development.

#### Gas Remaining
```rust
fn host_gas_remaining(env: FunctionEnvMut<HostEnv<S>>) -> u64
```
- **Return**: Gas yang tersisa
- **Purpose**: Allow contracts to check gas budget

#### Contract Address
```rust
fn host_contract_address(env: FunctionEnvMut<HostEnv<S>>, addr_ptr: u32) -> u64
```
- **Output**: Address kontrak yang sedang dieksekusi
- **Purpose**: Self-reference untuk contracts

## 5. Error Handling dan Keamanan

### 5.1 VMError Types

Tipe error yang dapat terjadi selama eksekusi.

```rust
pub enum VMError {
    OutOfGas,
    InvalidOpcode,
    StackOverflow,
    MemoryError,
    HostFunctionError(String),
    CompilationError(String),
    RuntimeError(String),
}
```

#### Kategori Error
- **Gas Errors**: OutOfGas, InsufficientGas
- **Execution Errors**: InvalidOpcode, StackOverflow
- **Memory Errors**: MemoryError, BoundsCheck
- **Host Errors**: HostFunctionError dengan detail

### 5.2 Security Measures

Langkah-langkah keamanan dalam VM.

#### Sandboxing
- **Isolated Execution**: WASM berjalan dalam sandbox
- **No Direct Access**: Tidak ada akses langsung ke host resources
- **Bounded Memory**: Memory usage dibatasi

#### Gas Limits
- **Execution Bounds**: Gas limits mencegah infinite loops
- **Resource Accounting**: Semua operasi di-charge gas
- **Economic Security**: Gas sebagai disincentive untuk abuse

#### Deterministic Execution
- **Reproducible Results**: Eksekusi selalu deterministik
- **No External Dependencies**: Tidak bergantung pada external state
- **Time-Independent**: Tidak ada time-based operations

## 6. Optimisasi Performa

### 6.1 JIT Compilation

Penggunaan Cranelift untuk just-in-time compilation.

#### Compilation Pipeline
- **WASM Parsing**: Parse dan validate bytecode
- **IR Generation**: Generate intermediate representation
- **Optimization**: Apply compiler optimizations
- **Code Generation**: Generate native code

### 6.2 Metering Efficiency

Optimisasi gas metering untuk minimal overhead.

#### Caching
- **Cost Lookup**: Cache opcode costs untuk fast access
- **Bulk Charging**: Charge multiple operations at once
- **Lazy Evaluation**: Defer charging sampai diperlukan

### 6.3 Memory Management

Efisien memory management untuk WASM execution.

#### Linear Memory
- **Bounds Checking**: Runtime bounds checking
- **Growing Memory**: Dynamic memory allocation
- **Garbage Collection**: Automatic cleanup

## 7. Integration dengan Sistem

### 7.1 State Manager Integration

VM terintegrasi dengan StateManager untuk persistent state.

#### State Transitions
- **Atomic Updates**: State changes dalam transaksi atomik
- **Verkle Proofs**: Cryptographic proof untuk state changes
- **Rollback Support**: Ability to revert state changes

### 7.2 Scheduler Integration

Koordinasi dengan parallel scheduler.

#### Transaction Ordering
- **Access Set Generation**: VM membantu generate access sets
- **Conflict Detection**: Aid dalam parallelization decisions
- **Batch Execution**: Support untuk batch contract execution

### 7.3 Consensus Integration

Integration dengan DAG consensus.

#### Block Processing
- **Transaction Validation**: VM validates contract transactions
- **Gas Fee Collection**: Accumulate fees dari contract execution
- **Finality**: Contracts achieve finality dengan block finality

## 8. Development dan Testing

### 8.1 Contract Development

Tools dan practices untuk development smart contracts.

#### WASM Compilation
- **Rust Contracts**: Compile Rust ke WASM
- **AssemblyScript**: Alternative untuk TypeScript-like syntax
- **Tooling**: Development tools dan debuggers

#### Testing Framework
- **Unit Tests**: Test individual functions
- **Integration Tests**: Test dengan full blockchain state
- **Gas Profiling**: Profile gas usage untuk optimization

### 8.2 Debugging Support

Fitur debugging untuk contract development.

#### Logging
- **Host Logging**: Log functions untuk debugging
- **Gas Tracing**: Trace gas consumption
- **State Inspection**: Inspect state changes

#### Error Reporting
- **Detailed Errors**: Comprehensive error messages
- **Stack Traces**: WASM stack traces untuk debugging
- **Performance Metrics**: Execution time dan gas usage

## Kesimpulan

Virtual Machine Klomang Core merupakan implementasi yang powerful dan aman untuk eksekusi smart contracts menggunakan WebAssembly. Melalui integrasi gas metering yang komprehensif, host functions yang lengkap, dan runtime Wasmer yang efisien, VM ini menyediakan environment yang ideal untuk development dan execution kontrak cerdas. Sistem ini mencapai keseimbangan optimal antara performa, keamanan, dan fleksibilitas, memungkinkan Klomang Core untuk mendukung aplikasi blockchain yang kompleks dan scalable.