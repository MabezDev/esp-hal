# `esp_hal::flash` — Implementation Plan (fast path to the stable surface)

Companion to [FLASH-DESIGN.md](FLASH-DESIGN.md), which is the authoritative whole-picture design (decision log in its [Appendix A](FLASH-DESIGN.md#appendix-a--decision-log); evidence in [Appendix B](FLASH-DESIGN.md#appendix-b--rom-capability-evidence) and [Appendix C](FLASH-DESIGN.md#appendix-c--esp-storage-internals-evidence)). Deferred/extracted material — the external SPI driver, chip configuration, custom-command evidence — is shelved in [FLASH-DEFERRED.md](FLASH-DEFERRED.md) and is on **no** path here. This document owns everything actionable: the PR breakdown, file inventory, verification checklist, and stabilization gates. Goal: land the blocking core, prove it through the real consumers, and stabilize — without waiting on the async work.

## Ship list — the stable surface being targeted

| Item | Notes |
|------|-------|
| `Flash<'d, Dm: DriverMode>` | `Dm` ships day one; only `Blocking` is usable on the fast path |
| `Flash::new(FLASH, Config)` | `DetectionFailed` instead of silent capacity-0; unstable `with_capacity` escape hatch (design [A13](FLASH-DESIGN.md#a13--no-chip-configuration-at-launch)) |
| `read` / `write` / `erase` / `capacity` / `apply_config` | chunked ROM ops, AutoPark multi-core default (lock-before-park), internal-RAM residency bounce (write sources *and* read destinations — flash and PSRAM are both cache-mapped); driver-owned bounce buffer, placement-validated at construction (`ConfigError::DriverPlacedInPsram`) |
| `Config`, `ConfigError`, `flash::Error` | `Config` is near-empty (multi-core strategy + unstable capacity override) and `#[non_exhaustive]` — grows only as real needs appear (design [A13](FLASH-DESIGN.md#a13--no-chip-configuration-at-launch)) |
| `peripherals.FLASH` stable | flipped at stabilization, not before |

Everything else in the design (async, encrypted, `chip_info`, `erase_chip`, the capacity-override setter, trait impls) lands **unstable**. Two of those are nonetheless *on* the fast path because the D2 consumer migration needs them: the embedded-storage trait impls (PR B) and encrypted access (PR C) — unstable, but D2-blocking. External SPI flash and chip configuration are out of scope entirely ([FLASH-DEFERRED.md](FLASH-DEFERRED.md)).

---

## PR breakdown

Changelog and migration-guide entries for every PR go in the **PR description** (structured sections), not `CHANGELOG.md` (per CONTRIBUTING.md).

### PR A — Driver core (size: L)

The stable surface, landed unstable via `unstable_driver!`.

**Scope**
- `esp-rom-sys`: bind `esp_rom_spiflash_read_user_cmd` (all chips — the private RDID/detection primitive, design [B2](FLASH-DESIGN.md#b2--read_user_cmd-availability-and-shape)) and `esp_rom_spiflash_erase_chip` (all chips — on S2 by adding the legacy-family ld alias Espressif omitted, `esp_rom_spiflash_erase_chip = SPIEraseChip`; design [B1](FLASH-DESIGN.md#b1--dedicated-rom-functions-and-the-s2-erase_chip-gap) correction / [A15](FLASH-DESIGN.md#a15--s2-erase_chip-bind-the-raw-rom-symbol)). Neither is bound today.
- `esp-hal/src/flash/{mod,driver,rom}.rs`: `Flash<'d, Dm>` (Blocking-only); every operation on its dedicated `esp_rom_spiflash_*` function — no backend enum, no custom-command machinery (design [A12](FLASH-DESIGN.md#a12--internal-only-scope-the-external-backend-splits-out)/[A13](FLASH-DESIGN.md#a13--no-chip-configuration-at-launch)).
- Capacity detection (`g_rom_flashchip` on ESP32, RDID via `read_user_cmd` elsewhere) → `ConfigError::DetectionFailed`; unstable `with_capacity` override as the escape hatch — capacity 0 never exists as a driver state.
- Chunked read/write/erase with per-chunk critical sections; `AutoPark` default with lock-before-park ordering (+ unstable `Error`/`Ignore` strategies); internal-RAM residency bounce for write sources and read destinations (flash *and* PSRAM; driver-owned bounce buffer, placement-validated at construction → `ConfigError::DriverPlacedInPsram`); near-empty `#[non_exhaustive]` `Config`.
- Free unstable extras (ROM fns already exist): `chip_info()`, `erase_chip()` with the agreed doc block (all 10 chips; S2 semantics check below).
- `esp-metadata/src/cfg.rs`: add a `flash` driver to `driver_configs!` (no such id exists today); `esp-metadata/devices/*/soc.toml`: `[device.flash]` entry + `cargo xtask update-metadata` (`flash_driver_supported`, README matrix row). FLASH singleton **stays unstable**.
- Module docs per design (doc_replace, `chip!()`, Limitations block).

**Tests**: new `hil-test/src/bin/flash.rs` (read/write/erase round-trip, capacity detection, bounds/alignment errors, PSRAM-buffer bounce and PSRAM-placed-driver rejection where PSRAM exists) — esp-storage's `storage.rs` test remains untouched and green in parallel. Plus three **go/no-go checks** — the first two moved forward from stabilization (cheap to run, and a failure changes stable contracts — they belong here, not at F), the third new with the S2 alias decision:

- dual-core read soundness: core 1 in an XIP hot loop, core 0 hammering `read` — verdict recorded in stable `read` docs (design [A4](FLASH-DESIGN.md#a4--multi-core-stable-default-autopark))
- ESP32 debug build (opt-level 0) of the flash HIL test — confirms dropping esp-storage's opt-level requirement (design [A2](FLASH-DESIGN.md#a2--esp32-opt-level-requirement-dropped): confirmed "in PR A, before anything stabilizes")
- S2 `SPIEraseChip` completion-wait semantics: one-off **manual** check on an S2 devkit (chip erase is destructive — not a CI HIL case; design [A15](FLASH-DESIGN.md#a15--s2-erase_chip-bind-the-raw-rom-symbol)); fallback is wrapping the call with the S2-aliased `esp_rom_spiflash_wait_idle`

**Exit**: lint-packages on c6/s3/esp32/p4, doc-tests, HIL green on at least c6 + s3 + esp32 — including the ESP32 opt-level-0 run and the dual-core read verdict.

### PR B — embedded-storage trait impls (size: S) — after A

- `esp-hal/Cargo.toml`: optional `embedded-storage-03` pulled in by `unstable`; **no new cargo features**. (`embedded-storage-async-04` lands together with the async impls — reserving an unused optional dep buys nothing and can trip unused-dep checks.)
- `ReadNorFlash` (READ_SIZE=1) / `NorFlash` (4/4096) / `MultiwriteNorFlash` for all `Dm`; `NorFlashError` mapping.
- Doc note: Multiwrite is non-encrypted-only.

### PR C — Encrypted access + MMU port (size: M) — after A, parallel with B

- Port `esp-storage/src/mmu.rs` (incl. P4 dual-map variant) as private `esp-hal/src/flash/mmu.rs`.
- `read_encrypted` / `write_encrypted` (unstable) — semantics matching esp-storage's whole-sector RMW wrapper (decrypted read → merge → erase → rewrite; the ROM primitive itself is 32-byte-granular, design [C4](FLASH-DESIGN.md#c4--encrypted-access-and-mmu)). Errors when flash encryption is off, as today. Bootloader's `EncryptedNorFlashRegion` keeps `WRITE_SIZE = 4096`.
- Extend `hil-test/src/bin/flash.rs` with the encrypted tests **in the encryption-off degenerate mode** — which is all `storage.rs` actually covers today (no CI device has encryption eFuses burned; a true encrypted round-trip is out of fleet scope).

### PR D1 — Bootloader flash seam + host-test decoupling (size: M) — independent of A/B/C; lands any time before D2

`FlashRegion` is hardwired to the concrete `esp_storage::FlashStorage<'d>` (struct field `partitions.rs:608`, `as_flash_region`, unconditional `esp_storage` references) — there is no seam to swap behind, so cutting one is real refactor work on a published crate, not an afterthought of the swap. Do it while still esp-storage-backed, so the host-test churn is reviewed in isolation and D2 becomes mechanical.

- **First, fix the gate itself**: `cargo xtask host-tests` runs the bootloader with only `std`, so `nor_flash_tests` (incl. the `WRITE_SIZE = 4096` wrapper assertions, `partitions.rs:1176-1194`) is compiled out today. Enable `embedded-storage` in the xtask invocation before touching anything that suite protects.
- Introduce the internal flash abstraction behind `FlashRegion`'s accesses + an in-crate `cfg(test)` mock; port the host tests onto the mock.
- `FlashRegion::write` takes ownership of the sector RMW (byte-granular public API preserved; today it leans on esp-storage's `Storage` RMW).
- esp-storage remains the hardware backend in this PR; `cargo xtask host-tests` stays green throughout.

### PR D2 — Consumer migration (size: M) — after B + C + D1

The proof-of-fire for the stable API; with D1 landed, mostly mechanical.

- `esp-bootloader-esp-idf`: swap the D1 seam's hardware backend to `esp_hal::flash::Flash<'d, Blocking>` (decision recorded here: concrete `Blocking`, not generic `Dm` — generics can come later, non-breaking for the bootloader's own semver). Drop `ReadStorage`/`Storage` impls on `FlashRegion` (breaking — migration guide entry) or reimplement atop the D1 RMW; keep `NorFlashRegion`/`EncryptedNorFlashRegion` working.
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
- Decide the deferred question: embedded-storage trait impls stabilize on the 0.3 suffix dep, or wait for 1.0 (design [A9](FLASH-DESIGN.md#a9--embedded-storage-trait-stabilization-deferred) — decision owner is this PR). **Required gate, not optional**: most of the practical de-unstabling value sits here — the dominant consumers use the traits, and the bootloader stays on unstable regardless (encrypted API).
- The capacity-override setter stays unstable unless a stable user has demonstrably needed it (design [A13](FLASH-DESIGN.md#a13--no-chip-configuration-at-launch)) — record the verdict either way.
- Migration guide + changelog entries via PR description per CONTRIBUTING.

## Critical path

```
A ──> B ──┐
  └─> C ──┼─> D2 ──> F (stabilization)
D1 ───────┘      └─> E (deprecation — off the critical path)
```

B, C and D1 are mutually parallel — D1 does not even depend on A and can start immediately. E never blocks F. The fast path to stable is **A → (B|C|D1) → D2 → F**; the only remaining long-tail design work (`into_async` + async trait impls) hangs off the unstable surface and can land any time after A without touching the stable timeline. Everything else that used to hang here (external SPI, custom commands) is out of scope — [FLASH-DEFERRED.md](FLASH-DEFERRED.md).

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
9. HIL: new `flash.rs` test (replaces `storage.rs`) — encrypted API covered in its **encryption-off degenerate mode** only (all CI devices have encryption disabled; a true encrypted round-trip needs an eFuse-burned board — out of fleet scope, recorded, not gated)
10. HIL: `flash.rs` on ESP32 built at opt-level 0 — confirms dropping the esp-storage opt-level requirement (design [A2](FLASH-DESIGN.md#a2--esp32-opt-level-requirement-dropped); first run in PR A)
11. HIL: OTA slot-switch round-trip — verifies the `otadata` update (explicit erase + write, guarding against sub-sector corruption/bricking after dropping the `Storage` RMW family)
12. qa-test: `multicore_flash` on S3
13. HIL: dual-core read soundness — core 1 in an XIP hot loop while core 0 hammers `read` (design [A4](FLASH-DESIGN.md#a4--multi-core-stable-default-autopark); first run in PR A)

## Stabilization gates (derived from the verification checklist above)

Checklist items not repeated here: fmt-packages (#5) runs in every PR's CI, and the example/bootloader builds (#7) are D2's own exit criteria. The semver-baseline gate comes from the design's stabilization-target section rather than the checklist.

- [ ] lint-packages: esp32c6, esp32s3, esp32, esp32p4
- [ ] doc-tests + documentation build
- [ ] HIL `flash` test green on all HIL-covered chips (encrypted API in its encryption-off mode — a true encrypted round-trip is out of fleet scope, design [C4](FLASH-DESIGN.md#c4--encrypted-access-and-mmu))
- [ ] Re-confirm the two PR A go/no-go verdicts still hold: ESP32 at opt-level 0 (design [A2](FLASH-DESIGN.md#a2--esp32-opt-level-requirement-dropped)) and dual-core read soundness (design [A4](FLASH-DESIGN.md#a4--multi-core-stable-default-autopark); outcome recorded in `read` docs) — **first run in PR A**, regression re-checks here
- [ ] HIL OTA slot-switch round-trip through the migrated bootloader (otadata explicit erase + write)
- [ ] qa-test `multicore_flash` on S3 (AutoPark default + unstable strategies)
- [ ] host-tests green on the bootloader's mock, with `embedded-storage` enabled in the xtask run (fixed in D1)
- [ ] embedded-storage trait-impl decision recorded (design [A9](FLASH-DESIGN.md#a9--embedded-storage-trait-stabilization-deferred)); capacity-override verdict recorded (design [A13](FLASH-DESIGN.md#a13--no-chip-configuration-at-launch))
- [ ] Semver baseline updated; `x. y. z` release notes drafted

## Explicitly off the fast path

| Deferred item | Where | Unblocked by |
|---------------|-------|--------------|
| `into_async` + async trait impls (chunk-yield internal async) | design, [Private internals](FLASH-DESIGN.md#private-internals-romrs-mmurs) | PR A |
| External SPI NOR driver (own type; the former `new_spi` backend) | [FLASH-DEFERRED.md](FLASH-DEFERRED.md) §1 | a committed implementer (split rationale: design [A12](FLASH-DESIGN.md#a12--internal-only-scope-the-external-backend-splits-out)) |
| Chip configuration (`ChipConfig` vocabulary: geometry/command/timing overrides) | [FLASH-DEFERRED.md](FLASH-DEFERRED.md) §2 | a real custom-chip need (design [A13](FLASH-DESIGN.md#a13--no-chip-configuration-at-launch)) |
| Internal custom commands (`common_command` binding + host-context hardware spike) | [FLASH-DEFERRED.md](FLASH-DEFERRED.md) §3 | chip configuration returning |
| Quad/Dual IO modes | [FLASH-DEFERRED.md](FLASH-DEFERRED.md) §1 | the external driver |
| Behavioral chip trait | [FLASH-DEFERRED.md](FLASH-DEFERRED.md) §4 | a real chip that needs it |
| Public flash mapping API (lookup over existing mappings / mapping creation) | [FLASH-DEFERRED.md](FLASH-DEFERRED.md) §5 | a real zero-copy consumer + an MMU-page ownership design (design [A16](FLASH-DESIGN.md#a16--public-mmap-cut-mmu-machinery-stays-private)) |

## File inventory (across all PRs)

Single reference table of everything the effort touches; per-PR scope above is authoritative for ordering.

| File | Action | PR |
|------|--------|----|
| `esp-hal/src/flash/mod.rs` | **Create** — public types, re-exports | A |
| `esp-hal/src/flash/driver.rs` | **Create** — `Flash` | A |
| `esp-hal/src/flash/rom.rs` | **Create** — private ROM wrappers, chunk loops, park ordering | A |
| `esp-hal/src/flash/mmu.rs` | **Create** — MMU port (encrypted reads; P4 dual-map) | C |
| `esp-hal/src/lib.rs` | Add flash module via `unstable_driver!` | A |
| `esp-rom-sys/src/rom/spiflash.rs` (+ S2 ld alias) | Add `esp_rom_spiflash_read_user_cmd` (all chips) + `esp_rom_spiflash_erase_chip` (all chips — S2 via new `SPIEraseChip` ld alias) | A |
| `esp-metadata/src/cfg.rs` | Add a `flash` driver to `driver_configs!` | A |
| `esp-metadata/devices/*/soc.toml` (×10) | Add `[device.flash]` driver entry (README matrix row) | A (+ F: `stable = true` on FLASH) |
| `esp-metadata-generated/` | `cargo xtask update-metadata` | A, F |
| `esp-hal/Cargo.toml` | Optional `embedded-storage-03` dep under `unstable` (no new features) | B (`embedded-storage-async-04` with the async work, off fast path) |
| `esp-bootloader-esp-idf/` | Flash seam + `cfg(test)` mock + `FlashRegion::write` RMW ownership; then backend swap, drop/reimplement `ReadStorage`/`Storage` impls (breaking — migration guide), keep `NorFlashRegion`/`EncryptedNorFlashRegion` working | D1, D2 |
| `xtask` (host-tests invocation) | Enable the bootloader's `embedded-storage` feature so `nor_flash_tests` compiles | D1 |
| `examples/peripheral/flash_read_write/` | Migrate to `esp_hal::flash` | D2 |
| `examples/ota/update/` | Migrate (writes app image via `FlashRegion::write`) | D2 |
| `hil-test/src/bin/flash.rs` | **Create** — replaces `storage.rs`; encrypted coverage (encryption-off mode) moves here | A (+ C: encrypted tests) |
| `hil-test/src/bin/storage.rs` | Retire once `flash.rs` covers it; HIL coverage stays continuous | D2 |
| `hil-test/src/bin/alloc_psram.rs` | `esp_storage::ll::spiflash_write` → `Flash::write` | D2 |
| `qa-test/src/bin/multicore_flash.rs` | Builder strategies → `Config` strategy | D2 |
| `qa-test/src/bin/qspi_flash.rs` | **Stays as-is** — prior art for the deferred external driver ([FLASH-DEFERRED.md](FLASH-DEFERRED.md) §1) | — |
| `hil-test/Cargo.toml`, `qa-test/Cargo.toml` | Feature wiring | A, D2 |
| `esp-storage/` | Deprecation notice (first workspace deprecation — mechanism TBD: README + crates.io description + doc banner) | E |
| `FLASH-DEFERRED.md` | Reference — deferred-material annex (external driver, chip config, custom-command evidence) | — |
