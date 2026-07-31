# `esp_hal::flash` — Implementation Plan (fast path to the stable surface)

Companion to [FLASH-DESIGN.md](FLASH-DESIGN.md), which is the authoritative whole-picture design (decision log in its [Appendix A](FLASH-DESIGN.md#appendix-a-decision-log); evidence in [Appendix B](FLASH-DESIGN.md#appendix-b-rom-capability-evidence) and [Appendix C](FLASH-DESIGN.md#appendix-c-esp-storage-internals-evidence)). Deferred/extracted material, including the external SPI driver, chip configuration, custom-command evidence, dedicated whole-chip erase, and async access, is shelved in [FLASH-DEFERRED.md](FLASH-DEFERRED.md) and is on **no** path here. This document owns everything actionable: the PR breakdown, file inventory, verification checklist, and stabilization gates. Goal: land the blocking core, prove it through the real consumers, and stabilize.

## Ship list — the stable surface being targeted

| Item | Notes |
|------|-------|
| `Flash<'d, Dm: DriverMode>` | `Dm` ships in the initial version; only `Blocking` is constructible |
| `Flash::new(FLASH, Config)` | Construction refreshes the cache through the ROM's `esp_rom_spi_flash_update_id` where it exists (9 of 10 targets; absent on ESP32) and decodes the cached three-byte JEDEC ID (design [B2](FLASH-DESIGN.md#b2-the-cached-jedec-id-and-its-provenance)). A sentinel, the ROM's W25Q16 default, or an unsupported density returns `ConfigError::UnknownFlashChip`; it does not panic and never yields a capacity-0 driver (design [A13](FLASH-DESIGN.md#a13-no-chip-configuration-at-launch)) |
| `apply_config(&mut self, &Config)` | Required by the developer guidelines and meaningful here: the only field is a software policy with no register side effects |
| `read` / `write` / `erase` / `capacity` | 4096-byte direct chunks and 256-byte staged chunks, per-operation cache/interrupt/other-core guard, AutoPark multi-core default (lock-before-park), one static internal-RAM bounce buffer for flash and PSRAM buffers; `#[ram]` scoped to the guard and ROM wrappers. Bounce storage may become configurable later, but is not part of this plan |
| `Config`, `ConfigError`, `flash::Error` | `Config` contains only the unstable multi-core strategy and is `#[non_exhaustive]`; `ConfigError` carries `UnknownFlashChip`. All three follow the guideline derive sets, including `Hash` and `defmt::Format` (design [A13](FLASH-DESIGN.md#a13-no-chip-configuration-at-launch)) |
| `peripherals.FLASH` stable | flipped at stabilization, not before |

Everything else in the design (encrypted access, `chip_info`, and trait impls) lands **unstable**. Two of those are nonetheless *on* the fast path because the D2 consumer migration needs them: the embedded-storage trait impls (PR B) and encrypted access (PR C), both unstable but D2-blocking. Dedicated whole-chip erase, async access, external SPI flash, and chip configuration are out of scope entirely ([FLASH-DEFERRED.md](FLASH-DEFERRED.md)).

---

## PR breakdown

Changelog and migration-guide entries for every PR go in the **PR description** (structured sections), not `CHANGELOG.md` (per CONTRIBUTING.md).

### PR A — Driver core (size: L)

The stable surface, landed unstable via `unstable_driver!`.

**Scope**
- `esp-rom-sys`: add a uniform accessor for the ROM-cached flash metadata. ESP32/ESP32-S2 expose the structure directly as `g_rom_flashchip`; the other eight targets expose `rom_spiflash_legacy_data`, which is a **pointer** to it, so the accessor needs one extra indirection there (design [B2](FLASH-DESIGN.md#b2-the-cached-jedec-id-and-its-provenance)). Also bind `esp_rom_spi_flash_update_id`, which the linker scripts already provide on 9 targets and ESP32 does not have. Move `esp-hal/src/psram/esp32.rs`, which already declares the struct and reads `device_id`, onto the shared accessor.
- `esp-hal/src/flash/{mod,driver,rom}.rs`: `Flash<'d, Dm>` (Blocking-only); every operation on its dedicated `esp_rom_spiflash_*` function, with no backend enum or custom-command machinery (design [A12](FLASH-DESIGN.md#a12-internal-only-scope-the-external-backend-splits-out)/[A13](FLASH-DESIGN.md#a13-no-chip-configuration-at-launch)).
- Capacity detection calls the ROM's `esp_rom_spi_flash_update_id` to refresh the cache where the symbol exists, then reads `device_id` and decodes physical capacity with the esptool-derived table. `esp-rom-sys` already provides the symbol on 9 targets (design [B2](FLASH-DESIGN.md#b2-the-cached-jedec-id-and-its-provenance)). The refresh touches SPI1, so it runs inside the operation guard.
- **ESP32 requires an ESP-IDF-compatible second-stage bootloader**, since its ROM neither identifies the chip nor has anything in `libesp_rom.a` to patch in. Settled: document the requirement in the module `Limitations` block and on `new()`; do not add a chip-specific detection path (design [A13](FLASH-DESIGN.md#a13-no-chip-configuration-at-launch)).
- **Re-map the byte order** before decoding: the cached value is manufacturer-first, while the esptool table and `esp-storage`'s decoder both read the raw `RDID` order, so manufacturer and density swap places (design [Byte order](FLASH-DESIGN.md#byte-order)). Porting the table verbatim mis-decodes every board.
- Detection deliberately ignores cached `chip_size`, which the bootloader overwrites with the image-header limit. Reject the sentinels (`0x000000`, `0xFFFFFF`, esptool's `0xFFFF3F`), the ROM's static W25Q16 default `0x001540EF`, and unsupported density encodings with `ConfigError::UnknownFlashChip`. There is no capacity fallback and no capacity-0 driver state.
- Chunk limits are split by path: 4096 bytes when the user buffer is already in internal RAM and aligned, 256 bytes when it is staged through the bounce buffer; erase uses sector or block chunks. The direct limit matches `esp-storage`; ESP-IDF allows 8192/16384 (design [B6](FLASH-DESIGN.md#b6-esp-idf-chunk-sizes)). Each low-level ROM call has an ESP-IDF-shaped operation guard that suspends and restores cache, local interrupts, the other core, and the flash lock on every return path. Writes and erases invalidate affected mappings before returning. `AutoPark` uses lock-before-park ordering for reads, writes, and erases, with unstable `Error`/`Ignore` strategies. The long-lived `Flash` type has no `Drop`; only the short-lived internal guard owns cleanup. One static 256-byte internal-RAM buffer stages flash, PSRAM, and unaligned user buffers. Its size or storage may become configurable later, but PR A exposes no configuration. `#[ram]` covers the guard and the ROM wrappers, not the public methods (design [A18](FLASH-DESIGN.md#a18-ram-residency-follows-the-cache-off-window)); PSRAM-backed stacks remain documented as unsupported.
- `apply_config` alongside `new`, per the developer guidelines.
- Free unstable extra: `chip_info()` reports the detected ID and physical capacity.
- `esp-metadata/src/cfg.rs`: add a `flash` driver to `driver_configs!` (no such id exists); `esp-metadata/devices/*/soc.toml`: `[device.flash]` entry + `cargo xtask update-metadata` (`flash_driver_supported`, README matrix row). FLASH singleton **stays unstable**.
- Module docs per design (doc_replace, `chip!()`, Limitations block). The Limitations block must state the ESP32 bootloader requirement and that PSRAM-backed stacks are unsupported.

**Tests**: new `hil-test/src/bin/flash.rs` (read/write/erase round-trip, balanced operation-guard cleanup on success and ROM error, bounds/alignment errors, direct and staged chunk boundaries, unaligned reads whose aligned superset crosses a boundary, and flash/PSRAM buffer staging where PSRAM exists). Verify the guard and ROM wrappers land in RAM sections and that public methods do not. esp-storage's `storage.rs` test remains untouched and green in parallel.

Detection needs its own assertion shape: compare decoded capacity against the **known** capacity of each board rather than asserting that decoding succeeded, because a reversed byte order still decodes on some IDs. Cover every supported chip family and applicable octal boot modes, and assert that a sentinel or unsupported density yields `Err(ConfigError::UnknownFlashChip)` rather than a panic.

Plus two **go/no-go checks** moved forward from stabilization because a failure changes stable contracts:

- ESP32 debug build (opt-level 0) of the flash HIL test — confirms dropping esp-storage's opt-level requirement (design [A2](FLASH-DESIGN.md#a2-esp32-opt-level-requirement-dropped): confirmed "in PR A, before anything stabilizes")
- Read/write throughput against `esp-storage` on one RISC-V and one Xtensa target - the chunk policy is visible through the stable API, so a large regression has to be caught before `read`/`write` are frozen

**Exit**: lint-packages on c6/s3/esp32/p4, doc-tests, HIL green on at least c6 + s3 + esp32, including the ESP32 opt-level-0 run and a dual-core read test proving `AutoPark` restoration.

### PR B — embedded-storage trait impls (size: S) — after A

- `esp-hal/Cargo.toml`: optional `embedded-storage-03` pulled in by `unstable`; **no new cargo features**.
- `ReadNorFlash` (READ_SIZE=1) / `NorFlash` (4/4096) / `MultiwriteNorFlash` for `Flash<'_, Blocking>`; `NorFlashError` mapping; every trait method placed in RAM.
- Doc note: Multiwrite is non-encrypted-only.

### PR C — Encrypted access + MMU port (size: M) — after A, parallel with B

- Port `esp-storage/src/mmu.rs` (incl. P4 dual-map variant) as private `esp-hal/src/flash/mmu.rs`.
- **Decide the alignment contract first.** ESP-IDF's public 16-byte contract and `esp-rom-sys`'s 32-byte note are both correct: the ESP32 ROM row is 32 bytes, and ESP-IDF bridges a 16-byte-aligned edge by reading the adjacent block back decrypted and re-encrypting it unchanged (design [B5](FLASH-DESIGN.md#b5-esp-idfs-encrypted-write-row-handling)). Either match ESP-IDF and implement that read-back on ESP32, or expose a per-chip alignment constant. Matching ESP-IDF is the recommendation, since the bootloader's OTA path is the dominant consumer. This is implementation work, not a HIL assertion, and it narrows the "no read-modify-write" rule to "no implicit erase, no change outside the requested range".
- `read_encrypted` / `write_encrypted` (unstable) follow ESP-IDF semantics (design [C4](FLASH-DESIGN.md#c4-encrypted-access-and-mmu)). `write_encrypted` requires previously erased flash and never erases implicitly. `write_encrypted` returns `NotSupported` when encryption is off. `#[ram]` scoping matches PR A.
- Extend `hil-test/src/bin/flash.rs` with encryption-off reads and encrypted writes behind an explicit test-only bypass of the production guard, the same `__test_esp_storage`-style cfg `esp-storage/src/encrypted.rs:51` uses today. Test chunk boundaries, erased-write behavior, alignment rejection, no implicit erase, and production `NotSupported`. On ESP32 specifically, exercise a 16-byte-aligned but not 32-byte-aligned write at both the start and the end of a range. No CI device has encryption eFuses burned, so a true encrypted round-trip remains out of fleet scope.

### PR D1 — Bootloader flash seam + host-test decoupling (size: M) — independent of A/B/C; lands any time before D2

The partition access code is hardwired to the concrete `esp_storage::FlashStorage<'d>` (struct field `partitions.rs:608`, `as_flash_region`, unconditional `esp_storage` references) — there is no seam to swap behind, so cutting one is real refactor work on a published crate, not an afterthought of the swap. Do it while still esp-storage-backed, so the host-test churn is reviewed in isolation and D2 can focus on the public partition-wrapper correction.

- **First, fix the gate itself**: `cargo xtask host-tests` runs the bootloader with only `std`, so `nor_flash_tests` is compiled out. Enable `embedded-storage` so the current surface is tested before D2 deliberately replaces it.
- Introduce the internal flash abstraction behind partition accesses plus an in-crate `cfg(test)` mock; port the host tests onto the mock.
- Keep the existing public partition wrappers and behavior in this seam-only PR. D2 owns their breaking correction.
- esp-storage remains the hardware backend in this PR; `cargo xtask host-tests` stays green throughout.

### PR D2 — Consumer migration (size: M) — after B + C + D1

The proof-of-fire for the stable API; with D1 landed, mostly mechanical.

- `esp-bootloader-esp-idf`: swap the D1 seam's hardware backend to `esp_hal::flash::Flash<'d, Blocking>` (decision recorded here: concrete `Blocking`, not generic `Dm` — generics can come later, non-breaking for the bootloader's own semver). Replace the dynamic partition wrapper and its second-stage NOR wrappers with direct checked conversions: `PartitionEntry::as_flash_region()` returns a plaintext `FlashRegion` that implements the blocking NOR traits, while `PartitionEntry::as_encrypted_flash_region()` returns an `EncryptedFlashRegion` with inherent read, erase, and encrypted-write methods only. Effective encryption uses partition flags, partition type, and hardware state; both accessors enforce the result. Remove `ReadStorage`/`Storage`, `NorFlashRegion`, and `EncryptedNorFlashRegion` with migration-guide entries. Higher-level OTA code chooses the accessor and keeps any runtime dispatch enum private. Encrypted writes use ESP-IDF prior-erase and 16-byte-alignment semantics.
- `examples/peripheral/flash_read_write`, `examples/ota/update`.
- `hil-test/src/bin/storage.rs` → retire in favor of `flash.rs` (or migrate, then delete the esp-storage variant).
- `hil-test/src/bin/alloc_psram.rs`: `esp_storage::ll::spiflash_write` → `Flash::write` (its source buffer is a stack local — no PSRAM subtlety in the migration).
- `qa-test/src/bin/multicore_flash.rs`: builder strategies → `Config` strategy.
- Feature wiring in `hil-test/Cargo.toml`, `qa-test/Cargo.toml`.

### PR E — esp-storage deprecation notice (size: S) — after D2

- README banner, crates.io description, crate-level doc banner pointing at `esp_hal::flash` + migration notes. First workspace deprecation — this sets the pattern.
- esp-storage stays published and untouched otherwise; it dies by attrition, not deletion.

### PR F — Stabilization (size: M) — after D2 + soak

- Run the stabilization gates (checklist below).
- Flip `stable = true` on FLASH in 10 `soc.toml`s + `update-metadata`.
- Move `flash` out of the `unstable_driver!` block in `esp-hal/src/lib.rs` into a plain `#[cfg(flash_driver_supported)] pub mod flash;`. `unstable_driver!` compiles the module out entirely without the feature, so this is a code move, not an attribute removal. Remove `unstable` gating from the remaining ship-list items; API baseline regeneration; README matrix to ✔️.
- Decide the deferred question: embedded-storage trait impls stabilize on the 0.3 suffix dep, or wait for 1.0 (design [A9](FLASH-DESIGN.md#a9-embedded-storage-trait-stabilization-deferred) — decision owner is this PR). **Required gate, not optional**: most of the practical de-unstabling value sits here — the dominant consumers use the traits, and the bootloader stays on unstable regardless (encrypted API).
- Migration guide + changelog entries via PR description per CONTRIBUTING.

## Critical path

```
A ──> B ──┐
  └─> C ──┼─> D2 ──> F (stabilization)
D1 ───────┘      └─> E (deprecation — off the critical path)
```

B, C and D1 are mutually parallel. D1 does not even depend on A and can start immediately. E never blocks F. The fast path to stable is **A → (B|C|D1) → D2 → F**. Async access, dedicated whole-chip erase, external SPI, and custom commands are deferred rather than hanging from this path. See [FLASH-DEFERRED.md](FLASH-DEFERRED.md).

## Verification checklist

The full verification inventory for the effort; individual PRs run the subset their exit criteria name, and the stabilization gates below re-run the load-bearing ones.

1. `cargo xtask lint-packages --packages esp-hal --chips esp32c6` (RISC-V baseline)
2. `cargo xtask lint-packages --packages esp-hal --chips esp32s3` (Xtensa + multi-core)
3. `cargo xtask lint-packages --packages esp-hal --chips esp32` (static ROM lib)
4. `cargo xtask lint-packages --packages esp-hal --chips esp32p4` (P4-specific dual-map MMU code)
5. `cargo xtask fmt-packages`
6. `cargo xtask run doc-tests <CHIP>` + `cargo xtask build documentation`
7. Build flash examples and `esp-bootloader-esp-idf`
8. `cargo xtask host-tests` — requires the D1 mock decoupling (and the D1 xtask `embedded-storage` fix)
9. HIL: new `flash.rs` test (replaces `storage.rs`) - encrypted reads use the encryption-off mode; encrypted writes use an explicit test-only bypass while production writes still reject encryption-off hardware. Verify prior-erase behavior, alignment rejection, no implicit erase, and the ESP32 16-byte-aligned row edges. All CI devices have encryption disabled, so a true encrypted round-trip needs an eFuse-burned board and remains out of fleet scope.
10. HIL: `flash.rs` on ESP32 built at opt-level 0 — confirms dropping the esp-storage opt-level requirement (design [A2](FLASH-DESIGN.md#a2-esp32-opt-level-requirement-dropped); first run in PR A)
11. HIL: OTA slot-switch round-trip — verifies the `otadata` update (explicit erase + write, guarding against sub-sector corruption/bricking after dropping the `Storage` RMW family)
12. qa-test: `multicore_flash` on S3
13. HIL: dual-core read guard — core 1 in an XIP hot loop while core 0 hammers `read`; verify `AutoPark` stops cache-dependent execution for each ROM call and restores core 1 afterward (design [A4](FLASH-DESIGN.md#a4-multi-core-stable-default-is-autopark); first run in PR A)
14. HIL: capacity decoded from the cached ID equals the known board capacity on every family; the ROM refresh via `esp_rom_spi_flash_update_id` links and returns the expected ID on the 9 targets that export it; on ESP32 the cached ID is correct under the ESP-IDF bootloader; a sentinel, the ROM W25Q16 default, or an unsupported density returns `Err(ConfigError::UnknownFlashChip)` (design [B2](FLASH-DESIGN.md#b2-the-cached-jedec-id-and-its-provenance); first run in PR A)
15. HIL: read/write throughput versus `esp-storage` on one RISC-V and one Xtensa target (design [B6](FLASH-DESIGN.md#b6-esp-idf-chunk-sizes); first run in PR A)

## Stabilization gates (derived from the verification checklist above)

Checklist items not repeated here: fmt-packages (#5) runs in every PR's CI, and the example/bootloader builds (#7) are D2's own exit criteria. The semver-baseline gate comes from the design's validation gates rather than the checklist.

- [ ] lint-packages: esp32c6, esp32s3, esp32, esp32p4
- [ ] doc-tests + documentation build
- [ ] HIL `flash` test green on all HIL-covered chips (encryption-off reads; test-bypass writes to erased aligned ranges; no implicit erase; production write rejection; ESP32 row-edge cases; true encrypted round-trip out of fleet scope, design [C4](FLASH-DESIGN.md#c4-encrypted-access-and-mmu))
- [ ] Re-confirm the PR A ESP32 opt-level-0 go/no-go verdict (design [A2](FLASH-DESIGN.md#a2-esp32-opt-level-requirement-dropped)), the dual-core read guard test proving `AutoPark` restoration (design [A4](FLASH-DESIGN.md#a4-multi-core-stable-default-is-autopark)), and the throughput comparison (design [B6](FLASH-DESIGN.md#b6-esp-idf-chunk-sizes))
- [ ] HIL OTA slot-switch round-trip through the migrated bootloader (otadata explicit erase + write)
- [ ] qa-test `multicore_flash` on S3 (AutoPark default + unstable strategies)
- [ ] host-tests green on the bootloader's mock, with `embedded-storage` enabled in the xtask run (fixed in D1)
- [ ] `flash` module moved out of `unstable_driver!`; `#[ram]` scoping still holds after the move
- [ ] embedded-storage trait-impl decision recorded (design [A9](FLASH-DESIGN.md#a9-embedded-storage-trait-stabilization-deferred)); cached identification validated against known board capacities on every supported family, with sentinel and unsupported-density inputs returning `ConfigError::UnknownFlashChip` (design [A13](FLASH-DESIGN.md#a13-no-chip-configuration-at-launch))
- [ ] Encrypted-write alignment contract recorded, and the `esp-rom-sys` binding's doc comment reconciled with it (design [B5](FLASH-DESIGN.md#b5-esp-idfs-encrypted-write-row-handling))
- [ ] Semver baseline updated; `x. y. z` release notes drafted

## Explicitly off the fast path

| Deferred item | Where | Unblocked by |
|---------------|-------|--------------|
| External SPI NOR driver (own type; the former `new_spi` backend) | [FLASH-DEFERRED.md](FLASH-DEFERRED.md) §1 | a committed implementer (split rationale: design [A12](FLASH-DESIGN.md#a12-internal-only-scope-the-external-backend-splits-out)) |
| Chip configuration (`ChipConfig` vocabulary: geometry/command/timing overrides) | [FLASH-DEFERRED.md](FLASH-DEFERRED.md) §2 | a real custom-chip need (design [A13](FLASH-DESIGN.md#a13-no-chip-configuration-at-launch)) |
| Internal custom commands (`common_command` binding + host-context hardware spike) | [FLASH-DEFERRED.md](FLASH-DEFERRED.md) §3 | chip configuration returning for a demonstrated internal need |
| Quad/Dual IO modes | [FLASH-DEFERRED.md](FLASH-DEFERRED.md) §1 | the external driver |
| Behavioral chip trait | [FLASH-DEFERRED.md](FLASH-DEFERRED.md) §4 | a real chip that needs it |
| Public flash mapping API (lookup over existing mappings / mapping creation) | [FLASH-DEFERRED.md](FLASH-DEFERRED.md) §5 | a real zero-copy consumer + an MMU-page ownership design (design [A16](FLASH-DESIGN.md#a16-public-mmap-cut-mmu-machinery-stays-private)) |
| Encrypted sector-overwrite helper | [FLASH-DEFERRED.md](FLASH-DEFERRED.md) §6 | a real user that cannot follow erase-before-write semantics |
| Dedicated whole-chip erase | [FLASH-DEFERRED.md](FLASH-DEFERRED.md) §7 | a concrete RAM-resident flasher or recovery consumer |
| Async internal flash access | [FLASH-DEFERRED.md](FLASH-DEFERRED.md) §8 | a consumer that benefits from between-chunk yielding |
| Configurable bounce storage | [FLASH-DEFERRED.md](FLASH-DEFERRED.md) §9 | measurements or a consumer that needs a different RAM/ROM-call trade-off |

## File inventory (across all PRs)

Single reference table of everything the effort touches; per-PR scope above is authoritative for ordering.

| File | Action | PR |
|------|--------|----|
| `esp-hal/src/flash/mod.rs` | **Create** — public types, re-exports | A |
| `esp-hal/src/flash/driver.rs` | **Create** — `Flash` | A |
| `esp-hal/src/flash/rom.rs` | **Create** — `#[ram]` ROM wrappers and operation guard, chunk loops (cache-enabled), static internal-RAM bounce buffer, park ordering | A |
| `esp-hal/src/flash/mmu.rs` | **Create** — MMU port (encrypted reads; P4 dual-map) | C |
| `esp-hal/src/lib.rs` | Add flash module via `unstable_driver!` | A (+ F: move out of `unstable_driver!`) |
| `esp-rom-sys/src/rom/spiflash.rs` | Add a cross-target accessor for ROM-cached flash metadata (pointer indirection on the 8 non-ESP32/S2 targets); bind `esp_rom_spi_flash_update_id` (9 targets, absent on ESP32); reconcile the `esp_rom_spiflash_write_encrypted` 32-byte doc comment with the chosen public contract | A (+ C) |
| `esp-hal/src/psram/esp32.rs` | Replace the private `g_rom_flashchip` declaration and `device_id` read with the shared accessor | A |
| `esp-metadata/src/cfg.rs` | Add a `flash` driver to `driver_configs!` | A |
| `esp-metadata/devices/*/soc.toml` (×10) | Add `[device.flash]` driver entry (README matrix row) | A (+ F: `stable = true` on FLASH) |
| `esp-metadata-generated/` | `cargo xtask update-metadata` | A, F |
| `esp-hal/Cargo.toml` | Optional `embedded-storage-03` dependency under `unstable` (no new features) | B |
| `esp-bootloader-esp-idf/` | D1: flash seam + `cfg(test)` mock. D2: backend swap; direct checked plaintext `FlashRegion` and `EncryptedFlashRegion` conversions; NOR traits only on plaintext; encrypted ESP-IDF erase-before-write semantics; higher-level OTA-owned runtime choice; removal of the dynamic and second-stage wrappers plus `ReadStorage`/`Storage` (breaking, with migration guide) | D1, D2 |
| `xtask` (host-tests invocation) | Enable the bootloader's `embedded-storage` feature so `nor_flash_tests` compiles | D1 |
| `examples/peripheral/flash_read_write/` | Migrate to `esp_hal::flash` | D2 |
| `examples/ota/update/` | Migrate through the OTA layer's effective-encryption choice | D2 |
| `hil-test/src/bin/flash.rs` | **Create** — replaces `storage.rs`; covers both chunk limits, RAM sections, buffer staging, capacity decoded against known boards, and encrypted erase/alignment semantics | A (+ C: encrypted tests) |
| `hil-test/src/bin/storage.rs` | Retire once `flash.rs` covers it; HIL coverage stays continuous | D2 |
| `hil-test/src/bin/alloc_psram.rs` | `esp_storage::ll::spiflash_write` → `Flash::write` | D2 |
| `qa-test/src/bin/multicore_flash.rs` | Builder strategies → `Config` strategy | D2 |
| `qa-test/src/bin/qspi_flash.rs` | **Stays as-is** — prior art for the deferred external driver ([FLASH-DEFERRED.md](FLASH-DEFERRED.md) §1) | — |
| `hil-test/Cargo.toml`, `qa-test/Cargo.toml` | Feature wiring | A, D2 |
| `esp-storage/` | Deprecation notice (first workspace deprecation — mechanism TBD: README + crates.io description + doc banner) | E |
| `FLASH-DEFERRED.md` | Reference — deferred-material annex (external driver, chip config, custom-command evidence) | — |
