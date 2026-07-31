# `esp_hal::flash` — Implementation Plan (fast path to the stable surface)

Companion to [FLASH-DESIGN.md](FLASH-DESIGN.md), which is the authoritative whole-picture design (decision log in its [Appendix A](FLASH-DESIGN.md#appendix-a-decision-log); evidence in [Appendix B](FLASH-DESIGN.md#appendix-b-rom-capability-evidence) and [Appendix C](FLASH-DESIGN.md#appendix-c-esp-storage-internals-evidence)). Deferred/extracted material, including the external SPI driver, chip configuration, custom-command evidence, dedicated whole-chip erase, and async access, is shelved in [FLASH-DEFERRED.md](FLASH-DEFERRED.md) and is on **no** path here. This document owns everything actionable: the PR breakdown, file inventory, verification checklist, and stabilization gates. Goal: land the blocking core, prove it through the real consumers, and stabilize.

## Ship list — the stable surface being targeted

| Item | Notes |
|------|-------|
| `Flash<'d, Dm: DriverMode>` | `Dm` ships in the initial version; only `Blocking` is constructible |
| `Flash::new(FLASH, Config)` | Logically infallible construction from the first-stage ROM's cached three-byte JEDEC ID; returns `Result` with an empty non-exhaustive `ConfigError` per the driver guideline. Invalid or unsupported cached metadata violates the platform invariant and panics instead of creating a capacity-0 driver (design [A13](FLASH-DESIGN.md#a13-no-chip-configuration-at-launch)) |
| `read` / `write` / `erase` / `capacity` | 256-byte read/write chunks, per-operation cache/interrupt/other-core guard, AutoPark multi-core default (lock-before-park), one static internal-RAM bounce buffer for flash and PSRAM buffers; all public methods and required call paths placed in RAM. Bounce storage may become configurable later, but is not part of this plan |
| `Config`, `ConfigError`, `flash::Error` | `Config` contains only the unstable multi-core strategy and is `#[non_exhaustive]`; `ConfigError` is empty and non-exhaustive for future configuration failures (design [A13](FLASH-DESIGN.md#a13-no-chip-configuration-at-launch)) |
| `peripherals.FLASH` stable | flipped at stabilization, not before |

Everything else in the design (encrypted access, `chip_info`, and trait impls) lands **unstable**. Two of those are nonetheless *on* the fast path because the D2 consumer migration needs them: the embedded-storage trait impls (PR B) and encrypted access (PR C), both unstable but D2-blocking. Dedicated whole-chip erase, async access, external SPI flash, and chip configuration are out of scope entirely ([FLASH-DEFERRED.md](FLASH-DEFERRED.md)).

---

## PR breakdown

Changelog and migration-guide entries for every PR go in the **PR description** (structured sections), not `CHANGELOG.md` (per CONTRIBUTING.md).

### PR A — Driver core (size: L)

The stable surface, landed unstable via `unstable_driver!`.

**Scope**
- `esp-rom-sys`: add a uniform accessor for the first-stage ROM's cached flash metadata: direct `g_rom_flashchip` data on ESP32/ESP32-S2 and `rom_spiflash_legacy_data` on newer chips (design [B2](FLASH-DESIGN.md#b2-three-byte-rdid)).
- `esp-hal/src/flash/{mod,driver,rom}.rs`: `Flash<'d, Dm>` (Blocking-only); every operation on its dedicated `esp_rom_spiflash_*` function, with no backend enum or custom-command machinery (design [A12](FLASH-DESIGN.md#a12-internal-only-scope-the-external-backend-splits-out)/[A13](FLASH-DESIGN.md#a13-no-chip-configuration-at-launch)).
- Capacity detection reads the ROM-cached `device_id` on every target and decodes physical capacity with the esptool-derived table. It deliberately ignores cached `chip_size`, which a second-stage bootloader may replace with the image-header limit. This uniform path avoids SPI1 register access, works for octal boot flash, and incorporates vendor startup corrections recorded in the cache. Zero/all-ones IDs and unsupported density encodings panic as violated platform invariants. There is no capacity fallback, populated `ConfigError` variant, or capacity-0 driver state.
- Read and write use private 256-byte maximum chunks; erase uses sector or block chunks. Each low-level ROM call has an ESP-IDF-shaped operation guard that suspends and restores cache, local interrupts, the other core, and the flash lock on every return path. Writes and erases invalidate affected mappings before returning. `AutoPark` uses lock-before-park ordering for reads, writes, and erases, with unstable `Error`/`Ignore` strategies. The long-lived `Flash` type has no `Drop`; only the short-lived internal guard owns cleanup. One static 256-byte internal-RAM buffer stages flash, PSRAM, and unaligned user buffers. Its size or storage may become configurable later, but PR A exposes no configuration. All public methods and required call paths carry `#[ram]`; PSRAM-backed stacks remain documented as unsupported.
- Free unstable extra: `chip_info()` reports the detected ID and physical capacity.
- `esp-metadata/src/cfg.rs`: add a `flash` driver to `driver_configs!` (no such id exists); `esp-metadata/devices/*/soc.toml`: `[device.flash]` entry + `cargo xtask update-metadata` (`flash_driver_supported`, README matrix row). FLASH singleton **stays unstable**.
- Module docs per design (doc_replace, `chip!()`, Limitations block).

**Tests**: new `hil-test/src/bin/flash.rs` (read/write/erase round-trip, cached three-byte ID and physical-capacity decoding, invalid-ID and unsupported-density panic checks, cache validation across supported chip families including applicable octal boot modes, balanced operation-guard cleanup on success and ROM error, bounds/alignment errors, 256-byte chunk boundaries, and flash/PSRAM buffer staging where PSRAM exists). Verify public methods and required helpers land in RAM sections. esp-storage's `storage.rs` test remains untouched and green in parallel. Plus one **go/no-go check** moved forward from stabilization because a failure changes stable contracts:

- ESP32 debug build (opt-level 0) of the flash HIL test — confirms dropping esp-storage's opt-level requirement (design [A2](FLASH-DESIGN.md#a2-esp32-opt-level-requirement-dropped): confirmed "in PR A, before anything stabilizes")

**Exit**: lint-packages on c6/s3/esp32/p4, doc-tests, HIL green on at least c6 + s3 + esp32, including the ESP32 opt-level-0 run and a dual-core read test proving `AutoPark` restoration.

### PR B — embedded-storage trait impls (size: S) — after A

- `esp-hal/Cargo.toml`: optional `embedded-storage-03` pulled in by `unstable`; **no new cargo features**.
- `ReadNorFlash` (READ_SIZE=1) / `NorFlash` (4/4096) / `MultiwriteNorFlash` for `Flash<'_, Blocking>`; `NorFlashError` mapping; every trait method placed in RAM.
- Doc note: Multiwrite is non-encrypted-only.

### PR C — Encrypted access + MMU port (size: M) — after A, parallel with B

- Port `esp-storage/src/mmu.rs` (incl. P4 dual-map variant) as private `esp-hal/src/flash/mmu.rs`.
- `read_encrypted` / `write_encrypted` (unstable) follow ESP-IDF semantics (design [C4](FLASH-DESIGN.md#c4-encrypted-access-and-mmu)). `write_encrypted` requires previously erased flash and 16-byte-aligned address and length; it never erases or performs sector RMW. Both operations use private 256-byte maximum chunks. `write_encrypted` returns `NotSupported` when encryption is off. All public encrypted methods and required helpers carry `#[ram]`.
- Extend `hil-test/src/bin/flash.rs` with encryption-off reads and encrypted writes behind an explicit test-only bypass of the production guard. Test 256-byte chunk boundaries, erased-write behavior, 16-byte alignment, no implicit erase, production `NotSupported`, and the local ROM binding's 32-byte claim against the 16-byte public contract. No CI device has encryption eFuses burned, so a true encrypted round-trip remains out of fleet scope.

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
- Remove `unstable` gating from the ship-list items; API baseline regeneration; README matrix to ✔️.
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
9. HIL: new `flash.rs` test (replaces `storage.rs`) - encrypted reads use the encryption-off mode; encrypted writes use an explicit test-only bypass while production writes still reject encryption-off hardware. Verify prior-erase behavior, 16-byte alignment, no implicit erase, and the ROM binding's 32-byte claim. All CI devices have encryption disabled, so a true encrypted round-trip needs an eFuse-burned board and remains out of fleet scope.
10. HIL: `flash.rs` on ESP32 built at opt-level 0 — confirms dropping the esp-storage opt-level requirement (design [A2](FLASH-DESIGN.md#a2-esp32-opt-level-requirement-dropped); first run in PR A)
11. HIL: OTA slot-switch round-trip — verifies the `otadata` update (explicit erase + write, guarding against sub-sector corruption/bricking after dropping the `Storage` RMW family)
12. qa-test: `multicore_flash` on S3
13. HIL: dual-core read guard — core 1 in an XIP hot loop while core 0 hammers `read`; verify `AutoPark` stops cache-dependent execution for each ROM call and restores core 1 afterward (design [A4](FLASH-DESIGN.md#a4-multi-core-stable-default-is-autopark); first run in PR A)

## Stabilization gates (derived from the verification checklist above)

Checklist items not repeated here: fmt-packages (#5) runs in every PR's CI, and the example/bootloader builds (#7) are D2's own exit criteria. The semver-baseline gate comes from the design's validation gates rather than the checklist.

- [ ] lint-packages: esp32c6, esp32s3, esp32, esp32p4
- [ ] doc-tests + documentation build
- [ ] HIL `flash` test green on all HIL-covered chips (encryption-off reads; test-bypass writes to erased 16-byte-aligned ranges; no implicit erase; production write rejection; ROM alignment mismatch resolved; true encrypted round-trip out of fleet scope, design [C4](FLASH-DESIGN.md#c4-encrypted-access-and-mmu))
- [ ] Re-confirm the PR A ESP32 opt-level-0 go/no-go verdict (design [A2](FLASH-DESIGN.md#a2-esp32-opt-level-requirement-dropped)) and the dual-core read guard test proving `AutoPark` restoration (design [A4](FLASH-DESIGN.md#a4-multi-core-stable-default-is-autopark))
- [ ] HIL OTA slot-switch round-trip through the migrated bootloader (otadata explicit erase + write)
- [ ] qa-test `multicore_flash` on S3 (AutoPark default + unstable strategies)
- [ ] host-tests green on the bootloader's mock, with `embedded-storage` enabled in the xtask run (fixed in D1)
- [ ] embedded-storage trait-impl decision recorded (design [A9](FLASH-DESIGN.md#a9-embedded-storage-trait-stabilization-deferred)); ROM-cached identification is validated across supported chip families and invalid or unsupported metadata triggers the documented construction panic (design [A13](FLASH-DESIGN.md#a13-no-chip-configuration-at-launch))
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
| `esp-hal/src/flash/rom.rs` | **Create** — private RAM-resident ROM wrappers, 256-byte chunk loops, static internal-RAM bounce buffer, park ordering | A |
| `esp-hal/src/flash/mmu.rs` | **Create** — MMU port (encrypted reads; P4 dual-map) | C |
| `esp-hal/src/lib.rs` | Add flash module via `unstable_driver!` | A |
| `esp-rom-sys/src/rom/spiflash.rs` | Add a cross-target accessor for ROM-cached flash metadata | A |
| `esp-metadata/src/cfg.rs` | Add a `flash` driver to `driver_configs!` | A |
| `esp-metadata/devices/*/soc.toml` (×10) | Add `[device.flash]` driver entry (README matrix row) | A (+ F: `stable = true` on FLASH) |
| `esp-metadata-generated/` | `cargo xtask update-metadata` | A, F |
| `esp-hal/Cargo.toml` | Optional `embedded-storage-03` dependency under `unstable` (no new features) | B |
| `esp-bootloader-esp-idf/` | D1: flash seam + `cfg(test)` mock. D2: backend swap; direct checked plaintext `FlashRegion` and `EncryptedFlashRegion` conversions; NOR traits only on plaintext; encrypted ESP-IDF erase-before-write semantics; higher-level OTA-owned runtime choice; removal of the dynamic and second-stage wrappers plus `ReadStorage`/`Storage` (breaking, with migration guide) | D1, D2 |
| `xtask` (host-tests invocation) | Enable the bootloader's `embedded-storage` feature so `nor_flash_tests` compiles | D1 |
| `examples/peripheral/flash_read_write/` | Migrate to `esp_hal::flash` | D2 |
| `examples/ota/update/` | Migrate through the OTA layer's effective-encryption choice | D2 |
| `hil-test/src/bin/flash.rs` | **Create** — replaces `storage.rs`; covers 256-byte chunking, RAM sections, buffer staging, defensive ID detection, and encrypted erase/alignment semantics | A (+ C: encrypted tests) |
| `hil-test/src/bin/storage.rs` | Retire once `flash.rs` covers it; HIL coverage stays continuous | D2 |
| `hil-test/src/bin/alloc_psram.rs` | `esp_storage::ll::spiflash_write` → `Flash::write` | D2 |
| `qa-test/src/bin/multicore_flash.rs` | Builder strategies → `Config` strategy | D2 |
| `qa-test/src/bin/qspi_flash.rs` | **Stays as-is** — prior art for the deferred external driver ([FLASH-DEFERRED.md](FLASH-DEFERRED.md) §1) | — |
| `hil-test/Cargo.toml`, `qa-test/Cargo.toml` | Feature wiring | A, D2 |
| `esp-storage/` | Deprecation notice (first workspace deprecation — mechanism TBD: README + crates.io description + doc banner) | E |
| `FLASH-DEFERRED.md` | Reference — deferred-material annex (external driver, chip config, custom-command evidence) | — |
