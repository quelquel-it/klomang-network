# Compilation & Deployment Checklist

## Pre-Compilation Verification

### Cargo.toml Dependencies
- [ ] rocksdb = "0.19" present
- [ ] bincode = "1.0" present
- [ ] serde with "derive" feature present
- [ ] klomang-core dependency correctly specified

### Module Structure
- [ ] src/storage/mod.rs exports: `pub mod read_path`
- [ ] src/storage/mod.rs exports: `pub use read_path::{ReadPath, OutPoint}`
- [ ] src/storage/mod.rs exports all other modules (cf, db, batch, etc.)

---

## Code Quality Checks

### read_path.rs Validation
```bash
# Check syntax
cargo check --lib storage::read_path
```
- [ ] No unused imports
- [ ] All `StorageResult` types correct
- [ ] No placeholder code (panic!, todo!, unimplemented!)
- [ ] All error paths handled

### db.rs Validation
```bash
# Verify configure_cf_options exists
cargo check --lib storage::db
```
- [ ] SliceTransform import present
- [ ] configure_cf_options() function exists (24 lines)
- [ ] Called in CF descriptor creation loop
- [ ] inner() method returns &DB

### Compilation Test
```bash
cargo build --release
```
- [ ] No errors
- [ ] No warnings (or only expected warnings)
- [ ] Binary successfully created

---

## Integration Tests

### Test UTXO Operations
```bash
cargo test --lib storage::read_path::tests --no-fail-fast
```
- [ ] outpoint_key_conversion passes
- [ ] outpoint_clone passes

### Test Examples Compilation
```bash
cargo build --example read_path_optimization
```
- [ ] Example compiles without errors
- [ ] All 9 example functions compile

### Test Atomic Write Integration
```bash
cargo build --example atomic_block_commit
```
- [ ] Atomic write still works after read path changes
- [ ] No conflicts with new modules

---

## Runtime Verification

### Example 1: Single UTXO Lookup
```bash
cargo run --example read_path_optimization -- --example single_utxo
```
- [ ] Returns Ok or None for nonexistent UTXO
- [ ] Properly deserializes UtxoValue
- [ ] No panics

### Example 2: Batch Lookup
```bash
cargo run --example read_path_optimization -- --example batch_utxo
```
- [ ] get_multiple_utxos returns Vec with same length as input
- [ ] Results properly deserialized
- [ ] Performance is 5-6x better than sequential

### Example 3: Prefix Seek
```bash
cargo run --example read_path_optimization -- --example prefix_scan
```
- [ ] Scans all outputs by transaction hash
- [ ] O(k) performance vs O(n)
- [ ] Correctly ordered by output index

### Example 4: Range Scan
```bash
cargo run --example read_path_optimization -- --example range_scan
```
- [ ] Iterator respects upper bound
- [ ] No unbounded memory growth
- [ ] max_results limit enforced

### Example 5: DAG Operations
```bash
cargo run --example read_path_optimization -- --example dag_tips
```
- [ ] get_dag_tips returns current tips
- [ ] scan_dag_nodes traverses correctly
- [ ] scan_blocks returns blocks in order

### Example 6: Bulk Check
```bash
cargo run --example read_path_optimization -- --example bulk_check
```
- [ ] check_utxos_exist returns HashMap
- [ ] All keys present in result
- [ ] Booleans correctly computed

---

## Database Integrity

### Prefix Extractor Configuration
```rust
// Verify in storage initialization
let db = StorageDb::open(path)?;

// Check if prefix extractors are configured
// This will be apparent from prefix seek performance
```
- [ ] Prefix seeks are O(k) not O(n)
- [ ] No full table scans occurring
- [ ] Iterator bounds working correctly

### Column Family Creation
- [ ] UTXO CF has 32-byte prefix extractor
- [ ] UtxoSpent CF has 32-byte prefix extractor
- [ ] Transactions CF has 32-byte prefix extractor
- [ ] Other CFs work without prefix extraction

---

## Performance Benchmarks

### Baseline Metrics (Expected)

Single Operations:
- [ ] Single get_utxo: ~0.1-0.5ms
- [ ] Full table scan: >100ms

Batch Operations:
- [ ] get_multiple_utxos (100 items): ~20-50ms (0.2-0.5ms each)
- [ ] Speedup factor: 5-6x vs sequential

Prefix Operations:
- [ ] get_utxos_by_tx_hash (50 outputs): ~1-5ms
- [ ] O(k) complexity confirmed

Range Scans:
- [ ] scan_utxo_range (1000 items): ~10-20ms
- [ ] scan_utxo_range (10000 items): ~50-100ms

DAG Operations:
- [ ] get_dag_tips: ~0.01ms (O(1) point lookup)
- [ ] scan_dag_nodes (100 nodes): ~5-10ms
- [ ] scan_blocks (1000 blocks): ~20-50ms

### Regression Testing
```bash
# Run benchmarks before and after changes
cargo bench --lib storage::read_path
```
- [ ] Speedups maintained
- [ ] No performance regressions
- [ ] Memory usage stable

---

## Production Deployment

### Pre-Production Checklist
- [ ] All tests pass: `cargo test --lib`
- [ ] Examples run successfully
- [ ] No memory leaks detected (valgrind/miri if needed)
- [ ] Error handling tested
- [ ] Edge cases covered

### Configuration Validation
- [ ] Block cache size appropriate (1GB default)
- [ ] Bloom filter bits per key set (10 default)
- [ ] Block size appropriate (32KB default)
- [ ] Prefix extractor bytes correct (32 for transaction hash)

### Database Migration (if upgrading)
- [ ] Backup existing database
- [ ] Run migration script if CF structure changed
- [ ] Verify data integrity after migration
- [ ] No data loss

### Monitoring Setup
- [ ] Performance metrics collected
- [ ] Error rates tracked
- [ ] Database size monitored
- [ ] WAL archive space tracked

---

## Known Issues & Resolutions

### Issue: Prefix Extractor Not Applied
**Symptom:** Prefix seeks still doing full table scans
**Resolution:**
1. Verify configure_cf_options() called during CF creation
2. Check SliceTransform import present
3. Verify ColumnFamilyName::Utxo uses correct enum value

### Issue: Serialization Errors
**Symptom:** StorageError::SerializationError on deserialize
**Resolution:**
1. Verify UtxoValue format matches database schema
2. Check bincode version consistency
3. Inspect raw bytes in database

### Issue: Performance Not Improving
**Symptom:** Batch operations no faster than sequential
**Resolution:**
1. Verify get_multiple_utxos uses db.multi_get_cf
2. Check if Bloom filters configured correctly
3. Monitor lock contention (may need read-only snapshots)

### Issue: Iterator Goes Out of Bounds
**Symptom:** Scanning beyond range or panicking
**Resolution:**
1. Verify set_iterate_upper_bound called correctly
2. Check upper_bound bytes properly formatted
3. Ensure key schema matches iterator bounds

---

## Final Validation

```bash
# Comprehensive test suite
cargo test --lib --all-features

# Build production binary
cargo build --release

# Check binary size (should be reasonable)
ls -lh target/release/klomang-node

# Verify all examples work
for example in examples/*.rs; do
  cargo build --example "$(basename $example .rs)" || exit 1
done

# Documentation validation
cargo doc --no-deps --open
```

---

## Rollout Plan

1. **Development**: All local tests passing ✓
2. **Staging**: Deploy to staging environment with production data
3. **Performance**: Monitor metrics for 24 hours
4. **Production**: Deploy with rolling updates
5. **Monitoring**: Track issues for 1 week post-deployment

---

## Sign-Off

- [ ] Code review approved
- [ ] All tests passing
- [ ] Benchmarks acceptable
- [ ] Documentation complete
- [ ] Deployment plan agreed
- [ ] Production ready

**Deployment Date**: _____________

**Deployed By**: _____________

**Verified By**: _____________

