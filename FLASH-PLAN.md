# `esp_hal::flash` — Implementation Plan (fast path to the stable surface)

Companion to `FLASH-DESIGN.md`, which is the authoritative whole-picture design (all ten review questions resolved 2026-07-16). This document breaks the **stable-target items** into ordered, reviewable PRs and marks everything else as explicitly off the fast path. Goal: land the blocking core, prove it through the real consumers, and stabilize — without waiting on the async/external-SPI/custom-command work.

## Ship list — the stable surface being targeted

| Item | Notes |
|------|-------|
| `FlashStorage<'d, Dm: DriverMode>` | `Dm` ships day one; only `Blocking` is usable on the fast path |
| `FlashStorage::new(FLASH, Config)` | `DetectionFailed` instead of silent capacity-0 |
| `read` / `write` / `erase` / `capacity` / `apply_config` | chunked ROM ops, AutoPark multi-core default, flash-residency write bounce |
| `Config`, `ConfigError`, `flash::Error` | per design |
| `Config::with_chip()` + capacity override | the answer to detection failure |
| `peripherals.FLASH` stable | flipped at stabilization, not before |

Everything else in the design (async, `new_spi`, custom commands, encrypted, `mmap`, `chip_info`, `erase_chip`, trait impls) lands **unstable** and is not gated on by the fast path except where noted (encrypted blocks the bootloader migration, which feeds a stabilization gate).

---

## PR breakdown

### PR A — Driver core (size: L)

The stable surface, landed unstable via `unstable_driver!`.

**Scope**
- `esp-rom-sys`: bind `esp_rom_spiflash_read_user_cmd` (all chips).
- `esp-hal/src/flash/{mod,storage,host}.rs`: `FlashStorage<'d, Dm>` (Blocking-only), ops-shaped `FlashBackend::Internal` implementing resolution step 1 only (dedicated `esp_rom_spiflash_*`) plus `read_id` via `read_user_cmd`; steps 2–3 return `NotSupported` stubs.
- Capacity detection (`g_rom_flashchip` on ESP32, RDID via `read_user_cmd` elsewhere) → `ConfigError::DetectionFailed`; capacity override via `with_chip`.
- Chunked read/write/erase with per-chunk critical sections; `AutoPark` default (+ unstable `Error`/`Ignore` strategies); flash-residency write bounce; hand-written `FlashChipConfig::Default`.
- Free unstable extras (ROM fns already exist): `chip_info()`, `erase_chip()` with the agreed doc block.
- `esp-metadata/devices/*/soc.toml`: `[device.flash]` entry + `cargo xtask update-metadata` (`flash_driver_supported`, README matrix row). FLASH singleton **stays unstable**.
- Module docs per design (doc_replace, `chip!()`, Limitations block).

**Tests**: new `hil-test/src/bin/flash.rs` (read/write/erase round-trip, capacity detection, bounds/alignment errors) — esp-storage's `storage.rs` test remains untouched and green in parallel.

**Exit**: lint-packages on c6/s3/esp32/p4, doc-tests, HIL green on at least c6 + s3 + esp32.

### PR B — embedded-storage trait impls (size: S) — after A

- `esp-hal/Cargo.toml`: optional `embedded-storage-03` (+ `embedded-storage-async-04` reserved, unused) pulled in by `unstable`; **no new cargo features**.
- `ReadNorFlash` (READ_SIZE=1) / `NorFlash` (4/4096) / `MultiwriteNorFlash` for all `Dm`; `NorFlashError` mapping.
- Doc note: consts never reflect geometry overrides; Multiwrite is non-encrypted-only.

### PR C — Encrypted access + MMU port (size: M) — after A, parallel with B

- Port `esp-storage/src/mmu.rs` (incl. P4 dual-map variant) as private internals.
- `read_encrypted` / `write_encrypted` (unstable, internal backend only) — semantics matching esp-storage (ROM encrypts whole sectors; bootloader's `EncryptedNorFlashRegion` assumes `WRITE_SIZE = 4096`).
- `mmap()` + `MappedFlash` (unstable) — same MMU machinery, so it rides along.
- Extend `hil-test/src/bin/flash.rs` with the encrypted round-trip (mirrors what `storage.rs` covers today).

### PR D — Consumer migration (size: L) — after B + C

The proof-of-fire for the stable API; every change here exercises it.

- `esp-bootloader-esp-idf`:
  - Swap `&mut esp_storage::FlashStorage<'d>` → `esp_hal::flash::FlashStorage<'d, Blocking>` (decision recorded here: concrete `Blocking`, not generic `Dm` — generics can come later, non-breaking for the bootloader's own semver).
  - `FlashRegion::write` owns the sector RMW (byte-granular public API preserved; today it leans on esp-storage's `Storage` RMW).
  - Drop `ReadStorage`/`Storage` impls on `FlashRegion` (breaking — migration guide entry) or reimplement atop the new RMW; keep `NorFlashRegion`/`EncryptedNorFlashRegion` working.
  - **Host tests**: decouple from esp-storage `emulation` — in-crate `cfg(test)` flash mock behind the `FlashRegion` seam. Prerequisite for killing esp-storage; `cargo xtask host-tests` must stay green.
- `examples/peripheral/flash_read_write`, `examples/ota/update`.
- `hil-test/src/bin/storage.rs` → retire in favor of `flash.rs` (or migrate, then delete the esp-storage variant).
- `hil-test/src/bin/alloc_psram.rs`: `esp_storage::ll::spiflash_write` → `FlashStorage::write`.
- `qa-test/src/bin/multicore_flash.rs`: builder strategies → `Config` strategy.
- Feature wiring in `hil-test/Cargo.toml`, `qa-test/Cargo.toml`.

### PR E — esp-storage deprecation notice (size: S) — after D

- README banner, crates.io description, crate-level doc banner pointing at `esp_hal::flash` + migration notes. First workspace deprecation — this sets the pattern.
- esp-storage stays published and untouched otherwise; it dies by attrition, not deletion.

### PR F — Stabilization (size: M) — after D + soak

- Run the stabilization gates (checklist below).
- Flip `stable = true` on FLASH in 10 `soc.toml`s + `update-metadata`.
- Remove `unstable` gating from the ship-list items; API baseline regeneration; README matrix to ✔️.
- Decide the deferred question: embedded-storage trait impls stabilize on the 0.3 suffix dep, or wait for 1.0 (design doc, resolved question 9 — decision owner is this PR).
- Migration guide + changelog entries via PR description per CONTRIBUTING.

## Critical path

```
A ──> B ──┐
  └─> C ──┴─> D ──> F (stabilization)
                └─> E (deprecation — off the critical path)
```

B and C are parallel. E never blocks F. The fast path to stable is **A → (B|C) → D → F**; the long-tail design work (async, `new_spi`, custom commands via `common_command`) hangs off the unstable surface and can land any time after A without touching the stable timeline.

## Stabilization gates (from FLASH-DESIGN.md § Verification)

- [ ] lint-packages: esp32c6, esp32s3, esp32, esp32p4
- [ ] doc-tests + documentation build
- [ ] HIL `flash` test green on all HIL-covered chips (incl. encrypted round-trip)
- [ ] HIL ESP32 at opt-level 0 (confirms dropping the opt-level requirement — resolved question 2)
- [ ] HIL dual-core read soundness: core 1 XIP hot loop + core 0 `read` hammer (resolved question 4); outcome recorded in `read` docs
- [ ] HIL OTA slot-switch round-trip through the migrated bootloader (otadata explicit erase + write)
- [ ] qa-test `multicore_flash` on S3 (AutoPark default + unstable strategies)
- [ ] host-tests green on the bootloader's new mock
- [ ] Semver baseline updated; `x. y. z` release notes drafted

## Explicitly off the fast path (design-doc pointers)

| Deferred item | Design section | Unblocked by |
|---------------|----------------|--------------|
| Custom-command execution (`common_command` binding, resolution steps 2–3 beyond `read_id`) | Custom chips / backend selection | PR A (stubs exist) |
| `new_spi` + `Spi` backend (incl. DMA/async plumbing) | Resolved question 8; qspi_flash prior art | PR A |
| `into_async` + async trait impls (chunk-yield internal, DMA external) | Private internals § async | PR A / Spi backend |
| Quad/Dual IO modes | Implementation State | Spi backend |
| Future behavioral chip trait | Custom chips § Future | a real chip that needs it |
