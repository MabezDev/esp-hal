# `esp_hal::flash` — Flash Storage Driver

**How to read this document**: the body is the design — linear and normative. Dense evidence, decision rationale, and esp-storage internals live in the appendices — [Appendix A (decision log)](#appendix-a--decision-log), [Appendix B (ROM capability evidence)](#appendix-b--rom-capability-evidence), [Appendix C (esp-storage internals)](#appendix-c--esp-storage-internals-evidence) — and are referenced inline as linked tags like [A4](#a4--multi-core-stable-default-autopark) or [C3](#c3--locking-and-multicore). (Inside code blocks the tags appear un-linked; they name the same entries.) Deferred/extracted material (external driver, chip overrides, custom-command evidence) is in [FLASH-DEFERRED.md](FLASH-DEFERRED.md).

---

Driver for the **internal boot flash** (SPI0/SPI1), replacing `esp-storage`. The constructor consumes the `FLASH` peripheral singleton; the only public type parameter is [`DriverMode`] (`Blocking`/`Async`), following the esp-hal convention that every applicable driver offers both blocking and async APIs.

**Scope ([A12](#a12--internal-only-scope-the-external-backend-splits-out))**: this driver is internal-only. An earlier revision folded external SPI NOR chips (SPI2/3) into the same type behind a runtime-erased backend enum; that erasure conflated two kinds of `NotSupported` — *contingent* chip gaps (S2's `erase_chip`: meaningful, liftable later — and indeed since closed, [A15](#a15--s2-erase_chip-bind-the-raw-rom-symbol)) and *structural* impossibilities (`mmap` needs the cache/MMU path and encrypted access the XTS-AES engine, both of which exist only on SPI0/SPI1 — no external chip can ever have them, and which backend you have is known at the constructor). The two backends also shared almost nothing beyond the operation list, so the erased `Config`/`Error`/Limitations smeared internal-only and external-only concerns across both. External flash is therefore a separate *future* driver, unified with this one through the embedded-storage traits rather than a shared concrete type. Everything external — and every other deferred piece — lives in [FLASH-DEFERRED.md](FLASH-DEFERRED.md).

Chip capability is still resolved **per operation**, and only operations the chip truly cannot execute fail, with `Error::NotSupported` at the point of use. After the scope cut — and the re-investigation of the suspected S2 `erase_chip` gap ([B1](#b1--dedicated-rom-functions-and-the-s2-erase_chip-gap) correction, [A15](#a15--s2-erase_chip-bind-the-raw-rom-symbol)) — **no capability gap remains**: every operation works on all 10 chips through the dedicated `esp_rom_spiflash_*` ROM functions (on S2, chip-erase through the raw `SPIEraseChip` export).

There is **no chip configuration** ([A13](#a13--no-chip-configuration-at-launch)): a boot flash the ROM booted is, by construction, drivable with the ROM's default command set — the "custom chip" user was always the external-chip user. `Config` ships `#[non_exhaustive]` and near-empty — the multi-core strategy plus an **unstable** capacity override, the escape hatch when detection fails — growing further parameters only as real needs appear. The full `ChipConfig` vocabulary is shelved in [FLASH-DEFERRED.md](FLASH-DEFERRED.md).

The async API runs the chunked blocking ROM ops, yielding only *between* chunks (XIP forbids suspending mid-ROM-call — the executor itself lives in the flash being written; between chunks the cache is live and yielding is safe).

Per DEVELOPER-GUIDELINES (API Surface, since #5854): **all of this lands unstable** (`unstable_driver!`), and the [stabilization target](#stabilization-target) is promoted deliberately once the API has settled.

---

## Baseline: what `esp-storage` already does

This design ports and reorganizes; it must not silently regress the current crate. Facts the rest of the document builds on:

- Read/write/erase go through ROM `esp_rom_spiflash_*` on **all 10 chips**; ESP32's incomplete ROM is patched by a static library in `esp-rom-sys`. There is **no** SPI1 register path for standard operations — only RDID capacity detection touches SPI1 registers. ([C1](#c1--rom-call-sites))
- The constructor already takes the `peripherals.FLASH` virtual singleton. It exists on all 10 chips in esp-metadata but is currently **unstable**.
- Erase and write are already **chunked**: per-sector/block/page ROM calls, each inside its own critical section. Interrupts re-enable between chunks; the cache is disabled only inside each ROM call. Worst-case interrupts-off window ≈ one 64 KiB block erase. ([C2](#c2--chunking-and-critical-sections))
- The critical section suppresses interrupts on the **executing core only** and cannot keep the other core's XIP off the flash. That gap is what `MultiCoreStrategy` exists for, and it guards write/erase only — **the existing read path is unguarded**. ([C3](#c3--locking-and-multicore))
- **Encrypted flash is load-bearing**: `esp-bootloader-esp-idf` routes every partition access through `is_effectively_encrypted()` and exposes `NorFlashRegion` / `EncryptedNorFlashRegion` wrapper views (#5857). The encrypted view's `WRITE_SIZE = 4096` is **esp-storage's wrapper contract, not a ROM constraint** — the ROM primitive is 32-byte-granular; the wrapper adds a whole-sector RMW with overwrite semantics (it erases internally) and errors when flash encryption is off. Hardware coverage exists only in the encryption-off degenerate mode. ([C4](#c4--encrypted-access-and-mmu))
- **MMU internals** exist for encrypted reads and cache invalidation, including the P4 dual-map variant. ([C4](#c4--encrypted-access-and-mmu))
- `pub mod ll` has external consumers (`hil-test/src/bin/alloc_psram.rs`).
- An `emulation` feature swaps the hardware layer for a stub; **esp-bootloader-esp-idf's host tests run on it** via `cargo xtask host-tests`.
- Capacity detection: ESP32 reads `g_rom_flashchip.chip_size` (RDID unreliable there); all others RDID via SPI1 registers. An unknown ID yields capacity **0**, after which every operation fails `OutOfBounds` with no indication why. ([C5](#c5--capacity-detection))
- `esp-storage/build.rs` rejects opt-level 0/1 on ESP32 — a timing requirement from when the flash routines were compiled from Rust. **Dropped** for the new driver. ([A2](#a2--esp32-opt-level-requirement-dropped))
- Buffers are passed straight to the ROM with no residency check — a flash-resident (`.rodata`) write source, or any PSRAM-resident buffer in either direction, is read/written while the cache is off. Latent footguns; this design closes all of them (see the buffer-residency rules under the write path). ([C6](#c6--buffers-and-read_size))

---

## The new design

Everything from here to the [stabilization target](#stabilization-target) is the new driver — normative, describing a proposed implementation rather than the existing one. From this point on, esp-storage appears only as ported semantics or evidence ([Appendix C](#appendix-c--esp-storage-internals-evidence)).

### Module structure

```
esp_hal::flash
├── mod.rs               — public types, re-exports
├── driver.rs            — Flash<'d, Dm> (mode type-state)
├── rom.rs               — private thin ROM wrappers: chunk loops, critical
│                          sections, park/unpark, bounce staging (ALL PRIVATE)
└── mmu.rs               — private MMU page mapping for encrypted reads
                           (ported from esp-storage, incl. P4 dual-map)
```

Public items in `mod.rs`:
- `Config`, `ConfigError`
- `Error`
- `ChipInfo`
- `MultiCoreStrategy` (multi-core chips, unstable)
- re-uses crate-level [`DriverMode`], [`Blocking`], [`Async`] (no re-export)

### Module documentation

Follows the current doc conventions: `procmacros::doc_replace` with `{before_snippet}`/`{after_snippet}` placeholders (no direct `crate::before_snippet!()`), ESP-IDF link via the crate-root `chip!()` macro (see `rng/mod.rs:1` for the module-level pattern).

```rust
#![cfg_attr(docsrs, procmacros::doc_replace(
    "documentation" => concat!("[ESP-IDF documentation](https://docs.espressif.com/projects/esp-idf/en/latest/", chip!(), "/api-reference/peripherals/spi_flash/index.html)")
))]
//! # Flash Storage
//!
//! ## Overview
//! Driver for the internal boot flash (SPI0/SPI1).
//! {documentation}
//!
//! ## Configuration
//! The default [`Config`] auto-detects flash geometry from ROM/RDID;
//! detection failure surfaces as [`ConfigError::DetectionFailed`] at
//! construction; the unstable capacity override on [`Config`] is the
//! escape hatch. Beyond that, `Config` carries no chip parameters (the
//! multi-core write strategy is its only other field, on multi-core
//! chips).
//!
//! ## Usage
//! With the `unstable` feature, implements
//! [`embedded_storage::nor_flash::ReadNorFlash`], [`NorFlash`] and
//! [`MultiwriteNorFlash`] (async equivalents on `Flash<'_, Async>`).
//! The legacy `ReadStorage`/`Storage` traits are intentionally not
//! implemented — use explicit `erase` then `write`.
//!
//! ## Examples
//! (short read example using {before_snippet}/{after_snippet})
//!
//! ## Implementation State
//! - Blocking API — first stabilization candidate
//! - Async API (via [`into_async`]) — chunked blocking ROM ops with
//!   inter-chunk yields
//! - Encrypted read/write (flash encryption)
//!
//! ## Limitations
//! - Drives the internal boot flash only. External SPI NOR chips on the
//!   general-purpose SPI masters are out of scope for this driver.
//! - During each ROM chunk, interrupts are disabled on the executing core;
//!   handlers that must run during flash operations (and everything they
//!   touch) must live in RAM and must not access flash or PSRAM.
//! - Flash operations must not be called from code whose stack lives in
//!   PSRAM (e.g. a task stack allocated from a PSRAM heap): the ROM
//!   routines execute on the caller's stack with the cache disabled. The
//!   driver validates its own buffers; it cannot validate your stack.
//! - No watchdog is fed while a ROM chunk runs; interrupts-off windows are
//!   chunk-bounded (worst ≈ one block erase) except `erase_chip`. Matches
//!   esp-storage behavior; a future API may run a user closure between
//!   chunks (e.g. to feed a watchdog).
//! - On multi-core chips, `write`/`erase` park the other core for the
//!   duration of each chunk (default `MultiCoreStrategy`). Parking is an
//!   unconditional hardware stall at an arbitrary instruction: whatever is
//!   running there — a lock holder, an interrupt handler, esp-radio's
//!   time-critical code — freezes in place, once per chunk. Unstable
//!   strategies opt out.
```

---

## Public types

### `Config`

```rust
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, BuilderLite)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Config {
    /// Unstable: selection of the multi-core write strategy.
    /// Defaults to `AutoPark` (see below).
    #[cfg(multi_core)]
    #[builder_lite(unstable)]
    multi_core_strategy: MultiCoreStrategy,

    /// Unstable: capacity override in bytes. `None` (default) = auto-detect
    /// from the flash chip itself. Set it when auto-detection fails
    /// (`ConfigError::DetectionFailed`); when set it takes precedence over
    /// detection.
    #[builder_lite(unstable)]
    capacity: Option<u32>,
}
```

`Config` is **deliberately minimal** ([A13](#a13--no-chip-configuration-at-launch)): both fields are unstable, so the *stable* Config surface starts empty, and on single-core chips the capacity override is its only field. It is `#[non_exhaustive]` precisely so parameters can be added as real needs appear. The capacity override ships unstable initially: detection failure must be an *error*, never a sentinel, but the rare unknown-RDID chip still needs a recourse — whether the setter ever stabilizes is decided with usage data (stabilization table below). The derived `Default` is valid: `None` = auto-detect, and `MultiCoreStrategy`'s default is `AutoPark`. (BuilderLite generates `with_capacity(u32)` — auto-wrapped in `Some` — plus `with_capacity_none()`.)

On multi-core chips the default strategy is **`AutoPark`** ([A4](#a4--multi-core-stable-default-autopark)): stable `write`/`erase` park the other core for the duration of each chunked ROM call and unpark it between chunks. **What parking is** — and the stable docs must say this — is an unconditional hardware CPU stall at an arbitrary instruction: the frozen core may hold a spinlock, sit mid-interrupt-handler, or be mid-timing-critical work, and it freezes there once per chunk (mechanism: [C3](#c3--locking-and-multicore)). Two implementation rules follow:

- **Lock-before-park**: the flash critical-section lock is acquired *before* parking the other core. esp-storage does the reverse, leaving a structural deadlock open if the frozen core held the lock ([C3](#c3--locking-and-multicore)); the FLASH singleton already makes that concretely unreachable, but the ordering closes it by construction and costs nothing.
- **Cooperative parking** (the other core spins at a safe point, ESP-IDF IPC-style) is the eventual answer for esp-rtos/esp-radio workloads — a future unstable `MultiCoreStrategy` variant, additive, not designed now.

`MultiCoreStrategy` (`Error`, and the `unsafe` `Ignore`) stays an **unstable** opt-in. Migration note required: esp-storage's default is `Error`/`OtherCoreRunning`.

**Reads** are not multicore-guarded (matching esp-storage). Expected sound — SPI0/SPI1 arbitrate in hardware, so the other core's cache refills interleave with SPI1 read transactions — but the claim is verified by a dedicated dual-core HIL case (core 1 in an XIP hot loop, core 0 hammering `read`) and the outcome recorded in stable `read`'s documentation. ([A4](#a4--multi-core-stable-default-autopark))

### `ConfigError`

Auto-detection is *not* infallible (the existing implementation maps an unknown RDID to capacity 0) — per the guidelines, an empty `ConfigError` is therefore wrong here:

```rust
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum ConfigError {
    /// The flash chip did not report a recognized JEDEC ID, so its
    /// capacity could not be determined. Provide the capacity explicitly
    /// via `Config::with_capacity`.
    DetectionFailed,
    /// The driver was constructed outside internal RAM — e.g. in a
    /// PSRAM-backed allocation. Flash operations run with the cache
    /// disabled, so the driver, which embeds its working buffer, must
    /// live in internal RAM.
    DriverPlacedInPsram,
}

impl core::fmt::Display for ConfigError { ... }
impl core::error::Error for ConfigError {}
```

Variant docs are written for the API user; the design rationale stays out here in prose. `DetectionFailed` replaces esp-storage's silent capacity-0 (detection failure is a real error, never an in-band state; recourse: the unstable `with_capacity`, [A13](#a13--no-chip-configuration-at-launch)). `DriverPlacedInPsram` is the construction-time placement check that makes the embedded bounce buffer sound — the driver validates its *own* buffer's address, since the buffer is driver-owned, not caller-supplied (buffer residency, under the write path). There is no capability variant: capability is per-operation, surfaced at the point of use as `Error::NotSupported`.

Deliberate deviation from the guidelines' "applying the default configuration must not fail": `Config::default()` (auto-detect) fails with `DetectionFailed` on a chip with an unknown RDID. The failure is environmental (unknown hardware), not combinatorial (invalid option mix) — the alternative is esp-storage's silent capacity-0, which this design explicitly rejects. The affected population is tiny — the ROM *booted* from the chip, so it is real, working flash that merely isn't in the RDID→capacity table — and the unstable `with_capacity` override is the recourse ([A13](#a13--no-chip-configuration-at-launch)). Detection failure is always a *real error*, never an in-band signal: capacity 0 does not exist as a driver state.

### `Error`

```rust
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Error {
    IoError,
    IoTimeout,
    /// The flash chip is locked and could not be unlocked.
    /// (esp-storage: `CantUnlock`)
    Locked,
    NotAligned,
    OutOfBounds,
    /// The requested operation is not available in the current
    /// configuration (e.g. an encrypted write while flash encryption
    /// is disabled).
    NotSupported,
    /// Unstable: only reachable via the unstable multi-core strategy.
    #[cfg(all(multi_core, feature = "unstable"))]
    OtherCoreRunning,
    /// An error that should not occur (e.g. an unexpected ROM return code).
    Unknown,
}

impl core::fmt::Display for Error { ... }
impl core::error::Error for Error {}
```

Module-scoped `flash::Error`, `Unknown` instead of `Other`, no payload (rationale: [A6](#a6--error-type-shape)). A `NorFlashError`/`NorFlashErrorKind` mapping is required by the traits (`NotAligned`/`OutOfBounds` map directly, rest `Other`), as in esp-storage. **`Unknown` must be loud**: the driver logs the raw ROM return code (and which ROM call produced it) via the crate's `defmt`/`log` machinery immediately before returning `Unknown` — the variant deliberately carries no payload, so that log line is the debugging path.

### `ChipInfo`

```rust
#[instability::unstable]
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ChipInfo {
    pub chip_id: u32,
    pub capacity: u32,
    pub sector_size: u32,
    pub block_size: u32,
    pub page_size: u32,
}
```

Geometry fields report the detected values (in practice the fixed ESP flash geometry: 256 B page, 4 KiB sector, 64 KiB block); there is no way to override them. `capacity` reflects the unstable override when set ([A13](#a13--no-chip-configuration-at-launch)).

---

## Detection and capability

**Detection / `chip_info` story — no register code anywhere:** the JEDEC ID comes from `esp_rom_spiflash_read_user_cmd(0x9F)` on all chips except ESP32, which reads the ROM data global `g_rom_flashchip` (`device_id`, `chip_size`) because hardware RDID is unreliable there. This replaces even esp-storage's one remaining register touch; capacity decode keeps the esptool table ([C5](#c5--capacity-detection)). `read_user_cmd` is a **private detection primitive** — it is not a user-facing custom-command surface ([A13](#a13--no-chip-configuration-at-launch); shape evidence in [B2](#b2--read_user_cmd-availability-and-shape)). An unknown JEDEC ID surfaces as `ConfigError::DetectionFailed` from `new()` unless the unstable `capacity` override is set — the override takes precedence over detection, and capacity 0 is never used as an in-band signal.

**Capability**: every operation resolves to its dedicated `esp_rom_spiflash_*` function on all 10 chips ([B1](#b1--dedicated-rom-functions-and-the-s2-erase_chip-gap)) — including `erase_chip` on S2, where esp-rom-sys adds the one legacy-family ld alias Espressif never bothered with (`esp_rom_spiflash_erase_chip = SPIEraseChip`; [A15](#a15--s2-erase_chip-bind-the-raw-rom-symbol)). A one-off S2 hardware check confirms the ROM routine's completion-wait semantics — and if it turns out not to wait, the S2-aliased `esp_rom_spiflash_wait_idle` wraps it, exactly as ESP32's patched implementation does. There is no SPI1 register fallback ([A1](#a1--no-spi1-register-exec-fallback)) and no custom-command machinery (shelved with the chip-override work — [FLASH-DEFERRED.md](FLASH-DEFERRED.md)).

A behavioral chip trait (quad-enable procedures, 32-bit addressing, erase suspend/resume) remains a future possibility, introduced only when a real chip needs it — sketch preserved in [FLASH-DEFERRED.md](FLASH-DEFERRED.md).

---

## `Flash<'d, Dm>`

The driver carries the esp-hal [`DriverMode`] type-state ([A14](#a14--drivermode-parameter-retained)). Constructors always build a `Blocking` driver; `into_async` / `into_blocking` flip between modes. Because esp-hal mode parameters carry **no default** (`I2c<'d, Dm: DriverMode>`, `Spi<'d, Dm: DriverMode>`), the `Dm` parameter must be part of the initial API — adding it after stabilization is a breaking change to every `Flash` annotation. The async *implementation* is promoted later (non-breaking). `Async` is `!Send` by construction (`Async(PhantomData<*const ()>)`, `lib.rs:557` — the `PhantomData<Dm>` field propagates it); for this driver that restriction is artificial (nothing pins to a core — accepted cost, [A14](#a14--drivermode-parameter-retained)). `Blocking` is `Send` (upheld in the internals: the driver owns all its state by value, no raw ROM pointers).

```rust
#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Flash<'d, Dm: DriverMode> {
    flash: FLASH<'d>,              // the consumed peripheral singleton
    bounce: AlignedPageBuffer,     // 256 B driver-owned bounce buffer (residency rules below)
    capacity: u32,
    unlocked: bool,
    _mode: PhantomData<Dm>,        // Dm = Async ⇒ driver is !Send
}
```

No `PeripheralGuard` field: `system::Peripheral` has no `Flash` variant (FLASH is virtual with no clock gate — absent from every `clocks.toml`), and the driver needs no Drop work.

```rust
// Constructors always produce a Blocking driver (esp-hal convention).
impl<'d> Flash<'d, Blocking> {
    pub fn new(
        flash: FLASH<'d>,
        config: Config,
    ) -> Result<Self, ConfigError>;   // DetectionFailed surfaces here (not as capacity-0);
                                      // DriverPlacedInPsram here too

    /// Convert into an async driver: chunked blocking ROM ops that yield
    /// between chunks. Type-state only — no interrupt handler is bound.
    #[instability::unstable]
    pub fn into_async(self) -> Flash<'d, Async>;
}

#[instability::unstable]
impl<'d> Flash<'d, Async> {
    pub fn into_blocking(self) -> Flash<'d, Blocking>;
    // async ops are exposed via the embedded_storage_async impls below
}

// Operations work in any mode — blocking calls remain available on an async
// driver (see esp_hal `DriverMode` docs: cheaper for small transfers).
impl<'d, Dm: DriverMode> Flash<'d, Dm> {
    pub fn apply_config(&mut self, config: &Config) -> Result<(), ConfigError>;

    pub fn read(&mut self, offset: u32, buffer: &mut [u8]) -> Result<(), Error>;
    pub fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Error>;
    pub fn erase(&mut self, from: u32, to: u32) -> Result<(), Error>;

    /// Erases the entire chip. Available on all 10 chips — on S2 via the
    /// ROM's raw `SPIEraseChip` export (B1 correction, A15).
    ///
    /// Understand what you are asking for:
    /// - This is a single, un-chunkable ROM call: multi-second, interrupts
    ///   disabled on the executing core for the entire duration. The async
    ///   version cannot yield — it is one giant blocking call in any mode —
    ///   and no watchdog can be fed while it runs.
    /// - Under the default `MultiCoreStrategy` the other core is parked for
    ///   the full duration: both cores are dead for seconds.
    /// - On return, the running program's own text is gone — the next cache
    ///   miss fetches erased flash. Only coherent from fully IRAM-resident
    ///   code (flasher-stub-style factory reset).
    #[instability::unstable]
    pub fn erase_chip(&mut self) -> Result<(), Error>;

    pub fn capacity(&self) -> usize;
    #[instability::unstable]
    pub fn chip_info(&self) -> ChipInfo;

    // Flash-encryption aware access. `write_encrypted` errors when flash
    // encryption is off — ported esp-storage semantics. Ports esp-storage's
    // encrypted.rs + mmu.rs, including write_encrypted's whole-sector RMW:
    // decrypted read → merge → erase sector → rewrite. NB overwrite
    // semantics (it erases internally), and the ROM primitive is
    // 32-byte-granular — the sector RMW is our wrapper contract; exposing
    // the aligned 32-byte primitive later is additive. (C4)
    // Load-bearing for esp-bootloader-esp-idf: `FlashRegion::read/write`
    // dispatch on `is_effectively_encrypted()`, and `EncryptedNorFlashRegion`
    // (WRITE_SIZE = 4096 — the wrapper's RMW granularity) delegates here.
    #[instability::unstable]
    pub fn read_encrypted(&mut self, offset: u32, buffer: &mut [u8]) -> Result<(), Error>;
    #[instability::unstable]
    pub fn write_encrypted(&mut self, offset: u32, data: &[u8]) -> Result<(), Error>;
}
```

**Write and read paths — buffer residency**: the check is inverted from "is it in flash?" to **"is it in internal RAM?"**. Any `write` source outside internal RAM is bounced through a RAM buffer — that covers flash `.rodata` *and* PSRAM, which is equally cache-mapped and equally broken with the cache off (under `psram_allocator!` every `Vec` is PSRAM-backed, so this is the common case, not a corner). Any `read` destination outside internal RAM is bounced chunk-wise through the same buffer. The existing implementation buffers unaligned sources. The bounce buffer is a fixed 256 B (one page, word-aligned) and **driver-owned** — embedded in the driver at construction, never a stack local: esp-storage's 4 KiB stack locals ([C6](#c6--buffers-and-read_size)) are a hazard this driver does not inherit, and a stack local would itself sit in PSRAM whenever the caller's stack does. `new()` validates the embedded buffer's address is internal RAM and rejects a PSRAM-placed driver with `ConfigError::DriverPlacedInPsram` — the check that makes driver-supplied buffering sound. To be explicit about the contract ([A17](#a17--plain-slice-buffers-no-aligned-buffer-types)): **user buffers never error for placement or alignment** — anything unaligned or outside internal RAM is staged through the bounce transparently, at a copy cost; word-aligned internal-RAM buffers take the zero-copy fast path (a performance note for the docs, not an API requirement).

### No public `mmap`

The bootloader establishes the running app's flash mappings before esp-hal ever executes; the driver neither creates nor owns them, and offers no public mapping API ([A16](#a16--public-mmap-cut-mmu-machinery-stays-private)). The MMU page-mapping machinery lives on privately in `mmu.rs`, where `read_encrypted` needs it. The two candidate public shapes — a lookup over *existing* mappings and a true mapping-creation API (which would need an MMU-page ownership story esp-hal doesn't have) — are recorded in [FLASH-DEFERRED.md](FLASH-DEFERRED.md) §5.

```rust
// Blocking embedded-storage traits — available in any mode. All trait impls
// are gated behind the `unstable` feature (see dependency policy below).
impl<Dm: DriverMode> ReadNorFlash for Flash<'_, Dm> {
    const READ_SIZE: usize = 1; // bytewise — unaligned reads buffered, aligned reads fast-path
                                // (replaces esp-storage's `bytewise-read` feature; NB default
                                // esp-storage is READ_SIZE = 4 — see C6 — so 4 → 1 is a
                                // loosening, but migration-note it)
}
impl<Dm: DriverMode> NorFlash for Flash<'_, Dm> {
    const WRITE_SIZE: usize = 4;    // word-aligned (ROM requirement)
    const ERASE_SIZE: usize = 4096; // sector — fixed across all ESP flash
}
impl<Dm: DriverMode> MultiwriteNorFlash for Flash<'_, Dm> {}
// MultiwriteNorFlash is only valid for non-encrypted access (doc note; the
// encrypted path never gets a Multiwrite impl).

// The legacy `ReadStorage`/`Storage` (RMW) family is intentionally NOT
// implemented — the NOR family is the modern surface. Callers needing
// sub-sector updates own the read-modify-write.

// Async embedded-storage traits — Async mode only.
impl embedded_storage_async::nor_flash::ReadNorFlash for Flash<'_, Async> { ... }
impl embedded_storage_async::nor_flash::NorFlash for Flash<'_, Async> { ... }
impl embedded_storage_async::nor_flash::MultiwriteNorFlash for Flash<'_, Async> {}
```

**Dependency/feature policy** — no new cargo features. esp-hal's pattern for pre-1.0 ecosystem crates is version-suffixed optional deps enabled by `unstable` (`embedded-io-06`, `embedded-io-07`, `embedded-can`, ...; only 1.0 crates like embedded-hal are unconditional). So: deps land as `embedded-storage-03` / `embedded-storage-async-04` (renamed), pulled in by `unstable`; all trait impls are `#[instability::unstable]`. Whether the impls later stabilize on the 0.3 dep (deliberate policy exception) or wait for embedded-storage 1.0 is **deferred to stabilization** ([A9](#a9--embedded-storage-trait-stabilization-deferred)) — the suffix pattern keeps either answer additive, so nothing is gained by committing early. The inherent `read`/`write`/`erase`/`capacity` API is the stable surface; this mirrors UART (stable inherent API, unstable embedded-io impls).

---

## Private internals (`rom.rs`, `mmu.rs`)

`rom.rs` owns the thin ROM wrappers (the `esp_rom_spiflash_*` set plus `read_user_cmd` for detection), the chunked read/write/erase loops with per-chunk critical sections, the lock-before-park ordering, and the bounce-buffer staging. `mmu.rs` is the esp-storage MMU port (encrypted reads, cache invalidation, P4 dual-map).

The totality principle from the original ops-shaped interface survives the backend enum's removal ([A3](#a3--ops-shaped-interface-principle-retained-backend-enum-dissolved)): every public operation is total and resolves to a dedicated ROM call on every chip ([A15](#a15--s2-erase_chip-bind-the-raw-rom-symbol) closed the last capability gap); `Err(NotSupported)` remains only for environmental states (encrypted write with encryption off). No dispatch is partial, so no `unreachable!()` and no panic path.

Async operations reuse the same internals: each chunked ROM call runs blocking (per page/sector/block, exactly the chunking esp-storage already does) and the future **yields between chunks**, bounding executor stall to one ROM call. Async futures are **chunk-granular cancel-safe** — dropping one mid-operation leaves the flash in a defined per-chunk state (every completed chunk is consistent; the remaining range is simply untouched/partially erased) — documented on the async impls when they land. `into_async` only changes the type-state: no interrupt handler is bound, no DMA is involved. Per the `DriverMode` contract, `Async` is `!Send`; `Blocking` is `Send`. This is orthogonal to `MultiCoreStrategy`, which governs the *other* core executing from flash during a write.

**Watchdogs / chunk boundaries**: no watchdog is fed inside a ROM chunk (interrupts are off; the ROM feeds nothing) — accepted as-is, matching esp-storage. If a real need appears, the additive answer is a future unstable `read_with`/`write_with`-style API that runs a user closure at each chunk boundary (kick a watchdog, update progress, ...) — noted, not designed now.

There is deliberately **no SPI1 register-exec path** ([A1](#a1--no-spi1-register-exec-fallback)): the driver calls only ROM functions bound in `esp-rom-sys`, and detection goes through `read_user_cmd` rather than registers.

---

## Host tests / emulation

esp-hal cannot build for the host, so the new driver cannot carry an equivalent of esp-storage's `emulation` backend (which is what esp-bootloader-esp-idf's host tests use — possible only because esp-hal is an *optional* dependency of esp-storage). Consequence for consumers: the bootloader's host tests must decouple from the concrete flash type.

---

## Stabilization target

Everything lands unstable. This table is the *target* stable surface for the eventual, deliberate stabilization PR (semver baseline update included):

| Item | Stable target | Stays unstable |
|------|---------------|----------------|
| `Config`, `ConfigError` | yes | |
| `Flash<'d, Dm: DriverMode>` — the mode parameter¹ | yes | |
| `Flash::new()` → `Flash<'d, Blocking>` | yes | |
| `Flash::apply_config()` | yes | |
| `Flash::read/write/erase/capacity` | yes | |
| `Error` | yes | |
| `peripherals.FLASH` singleton (currently unstable) | yes | |
| `Flash::erase_chip()` | | yes |
| `Flash::chip_info()`, `ChipInfo` | | yes |
| `Config::with_capacity()` (capacity override) | | yes — promoted only if a stable user demonstrably needs it |
| `read_encrypted` / `write_encrypted` | | yes |
| `ReadNorFlash`/`NorFlash`/`MultiwriteNorFlash` impls | | yes — decision deferred to stabilization ([A9](#a9--embedded-storage-trait-stabilization-deferred)) |
| `into_async()` / `into_blocking()` | | yes |
| `embedded-storage-async` trait impls (`Async` mode) | | yes |
| `MultiCoreStrategy`, `OtherCoreRunning` (multi-core) | | yes |

¹ The `Dm` type parameter must be part of the initial API: esp-hal mode parameters carry **no default**, so adding it later breaks every `Flash` annotation. Only the async *implementation* is deferred — promoting it later is non-breaking.

**Honest scope note**: the stable-only user is app code calling the inherent `read`/`write`/`erase`. The dominant consumers reach flash through the embedded-storage traits (sequential-storage, embassy-boot, littlefs adapters) or the encrypted API (esp-bootloader-esp-idf) — both unstable here, and the bootloader already forces `esp-hal/unstable` transitively via esp-storage's chip features. Most of the practical de-unstabling value therefore sits in the trait-impl decision, which is why [A9](#a9--embedded-storage-trait-stabilization-deferred)'s deferral is a **required gate decision at stabilization**, not an open-ended one.

---

## Appendix A — Decision log

Records of every design question raised in review and its resolution. Superseded entries keep their headings (anchor stability) with the superseding entry named; their extracted material lives in [FLASH-DEFERRED.md](FLASH-DEFERRED.md).

### A1 — No SPI1 register-exec fallback

The custom-chip user on ESP32/S2 drives an external data chip on SPI2/3 (prior art `qa-test/src/bin/qspi_flash.rs`); boot flash there is a standard chip on the ROM path. Internal operations that would need a register path were per-op `Error::NotSupported`. [A13](#a13--no-chip-configuration-at-launch) makes this trivially moot — there are no internal custom commands at all — but the decision stands on its own: no RAM-resident register bit-banging, ever; the driver calls only ROM functions.

### A2 — ESP32 opt-level requirement dropped

It was timing-related and predates #3688; the timing-sensitive code is precompiled in the ESP32 static ROM lib, and the remaining thin `#[ram]` ROM wrappers match the other chips, which never had the requirement. The new driver carries no build.rs check. Confirmation gate: run the new `flash.rs` HIL test on ESP32 in a debug build in PR A, before anything stabilizes.

### A3 — Ops-shaped interface (principle retained; backend enum dissolved)

Originally, the backend interface was designed around user operations rather than ROM entry points, dissolving an `exec` + `unreachable!()` shape. The scope was cut by [A12](#a12--internal-only-scope-the-external-backend-splits-out) and [A13](#a13--no-chip-configuration-at-launch). The backend enum left with the external driver and the multi-primitive resolution left with custom commands (full three-step table preserved in [FLASH-DEFERRED.md](FLASH-DEFERRED.md)), but the principle stands: every operation is total — a dedicated ROM call or `Err(NotSupported)` — and no panic arm exists.

### A4 — Multi-core stable default: AutoPark

`Error` is unshippable as a stable default (strategy selection is unstable, so stable users would have no recourse), and a blocking cross-core critical section cannot exist for non-cooperating XIP code. Stable contract: write/erase briefly halt the other core, documented. `Error`/`Ignore` stay unstable opt-ins; migration note vs esp-storage's `Error` default. Reads stay unguarded, backed by a dual-core HIL verdict recorded in stable `read` docs.

### A5 — mmap is an inherent method

This was an `InternalFlashExt` extension trait; dropped — single implementor, and per-op `NotSupported` was the design's uniform capability model. `MappedFlash` stays. With [A12](#a12--internal-only-scope-the-external-backend-splits-out), `mmap` no longer has any `NotSupported` path at all — the structural carve-out was the split's smoking gun. [A16](#a16--public-mmap-cut-mmu-machinery-stays-private) subsequently removed public `mmap` entirely; the inherent-method conclusion carries over to any future revival.

### A6 — Error type shape

`flash::Error` (module-scoped, per convention; esp-storage's `FlashError` name is not carried over), `Unknown` not `Other` per esp-hal convention, no `i32` payload (an unexpected ROM return code is a bug to report, not data to branch on), `#[non_exhaustive]`. The "SPI-backend variants added later" note is superseded by [A12](#a12--internal-only-scope-the-external-backend-splits-out): the external driver owns its own error design.

### A7 — erase_chip allowed, unstable, loud docs

`NotSupported` would violate the per-op capability law (the chip can execute it), and `unsafe` would be incoherent (safe `erase` can already wipe the running app). Doc block states: single un-chunkable multi-second ROM call with interrupts off; other core parked the whole time under `AutoPark`; the caller's own text is erased on return — only coherent from IRAM-resident code. Correction: ESP32-S2's ROM lacks the aliased function entirely — on S2, `NotSupported` *is* the per-op-honest answer; binding raw `SPIEraseChip` is additive ([B1](#b1--dedicated-rom-functions-and-the-s2-erase_chip-gap)). Re-corrected by [A15](#a15--s2-erase_chip-bind-the-raw-rom-symbol): esp-rom-sys supplies the missing alias itself — `erase_chip` is available on all 10 chips and no `NotSupported` case remains for it.

### A8 — new_spi takes a pre-configured Spi driver (superseded)

Superseded by [A12](#a12--internal-only-scope-the-external-backend-splits-out). The external backend left the driver. The decision text and its open questions move to [FLASH-DEFERRED.md](FLASH-DEFERRED.md) — where the split *un-freezes* the bus-ownership question the erasure had closed (a standalone external driver can take an embedded-hal `SpiDevice` and share the bus).

### A9 — embedded-storage trait stabilization deferred

Impls stay `#[instability::unstable]`; the 1.0-policy-vs-exception call belongs to the stabilization PR, with usage data in hand. The version-suffix dep pattern (`embedded-storage-03`) keeps either outcome additive.

### A10 — ll module stays private

No public `esp_hal::flash::ll`: it would bypass bounds checks, unlock state, and `MultiCoreStrategy` under esp-hal's name, and the only public-`ll` precedent (`clock::ll`) exists because there is no full driver there. `esp-rom-sys` is the designated raw escape hatch; `alloc_psram.rs` migrates to `Flash::write`. Thin ROM wrappers live on as a private module; making them public later is additive if a real user appears.

### A11 — Driver-built host context (superseded, evidence preserved)

Superseded by [A13](#a13--no-chip-configuration-at-launch). With chip configuration cut, `spi_flash_hal_common_command` has no consumer. The settled investigation — the ROM's `esp_flash_default_chip` is never initialized under esp-hal's boot flow; a driver using `common_command` must build its own host context — is preserved in full in [FLASH-DEFERRED.md](FLASH-DEFERRED.md), together with its residual hardware validation spike.

### A12 — Internal-only scope: the external backend splits out

The runtime-erased backend enum conflated two kinds of `NotSupported`: *contingent* chip gaps (S2 `erase_chip` — a meaningful operation, liftable additively) and *structural* impossibilities (`mmap` needs the cache/MMU path, encrypted access the XTS-AES engine — both exist only on SPI0/SPI1; no external chip can ever have them, and which backend exists is known at the constructor — a compile-time fact the erasure discarded, then partially recovered at runtime). The backends also shared almost nothing beyond the operation list: multi-core parking, residency bounce, cache-off critical sections and the entire Limitations block are internal-only; DMA error variants and genuinely interrupt-driven async are external-only — the shared `Config`/`Error` smeared both directions. The erasure additionally forced exclusive SPI-bus ownership (a generic `SpiDevice` parameter cannot exist on an erased type). Resolution: `Flash` is internal-only and keeps its name (it is *the* system flash); the external driver becomes its own future type, unified with this one through the embedded-storage traits — the ecosystem's own mechanism for "generic over storage" — not through a shared concrete type. All deferred material moves to [FLASH-DEFERRED.md](FLASH-DEFERRED.md) (external design, `ChipConfig` vocabulary, custom-command evidence).

### A13 — No chip configuration at launch

A boot flash the ROM booted is, by construction, drivable with the ROM's default command set — the "custom chip" user was always the external-chip user, on every target (generalizing [A1](#a1--no-spi1-register-exec-fallback)'s logic from ESP32/S2 to all 10 chips). `Config` therefore ships `#[non_exhaustive]` and empty apart from `multi_core_strategy` (multi-core chips only), growing parameters only as real needs appear. Cut with it: `ChipConfig` (whole type → [FLASH-DEFERRED.md](FLASH-DEFERRED.md)), `ConfigError::InvalidChipConfig`, resolution steps 2–3, the `common_command` binding, and the A11 hardware spike. A review refinement requires detection failure to be a *real error* from `new` — esp-storage's capacity-0 in-band signal is explicitly rejected — and the recourse ships initially as an **unstable** `with_capacity` override rather than waiting for a user to get stuck. Whether that setter ever stabilizes is a stabilization-time call with usage data; the stable Config surface still starts empty.

### A14 — DriverMode parameter retained

Grilled because its strongest original justification — interrupt/DMA-driven async on the external backend — left with [A12](#a12--internal-only-scope-the-external-backend-splits-out). Retained anyway: crate-wide convention (every async-capable esp-hal driver carries `Dm`), initial-surface necessity (mode parameters have no default, so a retrofit breaks every annotation), and a real async need remains (inter-chunk yields keep an embassy executor live during flash writes — embassy-boot / async OTA). Accepted cost: the shared `Async` type-state is `!Send` by construction — artificial for this driver, since nothing binds a handler or pins to a core.

### A15 — S2 erase_chip: bind the raw ROM symbol

Challenged in review — "IDF gets all chips to work, so we can too" — with pre-compiling the function (as done for ESP32) as the suspected mechanism. The investigation ([B1](#b1--dedicated-rom-functions-and-the-s2-erase_chip-gap) correction) found the reality simpler than the patch hypothesis: the S2 ROM routine exists (`SPIEraseChip = 0x400170ec`); Espressif's legacy-alias ld file omits only chip-erase because no IDF code anywhere calls the legacy erase-chip API (IDF's own chip erase runs through the app-compiled flash HAL); and the compiled ROM patch defines erase_chip for ESP32 only — there is nothing to precompile for S2. Resolution: esp-rom-sys adds `esp_rom_spiflash_erase_chip = SPIEraseChip` for S2, making `erase_chip` available on all 10 chips. A one-off S2 hardware check (manual — chip erase is destructive, not a CI HIL case) confirms completion-wait semantics, with the S2-aliased `esp_rom_spiflash_wait_idle` as the wrap-around fallback (mirroring ESP32's patched implementation: wait-idle → CE → wait-idle). This closes the last per-op capability gap: `Error::NotSupported` retains only environmental cases (encrypted write with encryption off).

### A16 — Public mmap cut; MMU machinery stays private

Raised in review: the flash mappings for the running app are established by the bootloader before esp-hal ever runs — the driver neither creates nor owns them — so a public `mmap` *operation* misrepresents the driver's authority. The refinement: the app image's segments are indeed pre-mapped, but arbitrary regions (e.g. an asset partition) are *not* — a true mapping-creation API is what IDF's `spi_flash_mmap`/`esp_partition_mmap` provide, and it requires an MMU-page ownership story (free-slot allocation, coexistence with PSRAM init and the transient encrypted-read mappings) that esp-hal does not have and that this driver should not hand-wave in as a rider on the encrypted port. Resolution: public `mmap()`/`MappedFlash` leave the surface; the MMU machinery (`mmu.rs`) stays as private internals for `read_encrypted`. Both public shapes are recorded in [FLASH-DEFERRED.md](FLASH-DEFERRED.md) §5 for when a real consumer appears: a *lookup* over existing mappings (IDF `spi_flash_phys2cache` analogue — cheap, borrows the driver like `MappedFlash` did) and the full mapping-creation API (needs the MMU allocator design).

### A17 — Plain-slice buffers; no aligned-buffer types

Raised in review: should `read`/`write` demand buffer placement/alignment (documented contract, or an aligned-slice type in the signatures) instead of erroring? Neither, and the design already does neither: user buffers *never* error for placement or alignment — the driver-embedded bounce buffer stages any source/destination that is unaligned or outside internal RAM, transparently, at a copy cost. `DriverPlacedInPsram` concerns only the placement of the *driver itself*, which embeds that bounce buffer: a scratch buffer in PSRAM cannot stage anything, and no copy can fix it, hence the constructor check. Aligned-buffer *types* were considered and rejected: the embedded-storage traits fix the signatures at plain `&[u8]`/`&mut [u8]`, so the dominant consumers (sequential-storage, embassy-boot, littlefs adapters) could never supply a special type — and alignment here is a fast-path optimization, not a hard requirement (unlike DMA, where esp-hal's dedicated buffer types exist because copying would defeat DMA's purpose). The docs state the fast path — word-aligned internal-RAM buffers avoid the copy — as a performance note.

No open questions — A1–A17 resolved; superseded entries (A8, A11, parts of A3/A5/A6/A7) are marked above and their material preserved in [FLASH-DEFERRED.md](FLASH-DEFERRED.md). One open hardware task: the S2 `SPIEraseChip` semantics check ([A15](#a15--s2-erase_chip-bind-the-raw-rom-symbol)).

---

## Appendix B — ROM capability evidence

Per-chip ROM symbol facts, verified against `esp-rom-sys/ld/`, IDF's `components/esp_rom/*/ld/`, and the IDF sources. The `common_command` evidence (former B3) moved to [FLASH-DEFERRED.md](FLASH-DEFERRED.md) with the custom-command work.

### B1 — Dedicated ROM functions and the S2 erase_chip gap

Present on **all 10 chips** for read/write/erase-sector/erase-block/unlock/write-encrypted — the default command set works everywhere. Exception: **`esp_rom_spiflash_erase_chip` is 9/10 — absent on ESP32-S2**, whose ROM exports only the raw, un-aliased `SPIEraseChip = 0x400170ec` (`esp32s2.rom.ld:612`) and has no patch lib (`esp-rom-sys/libs/esp32s2/` is a placeholder). (Superseded — see the correction below: the missing piece is the *alias*, not the routine.) ESP32's incomplete ROM is patched by `esp-rom-sys/libs/esp32/libesp_rom.a` (pinned to IDF v5.3.1, #3688), which *does* define `esp_rom_spiflash_erase_chip`.

**Correction ([A15](#a15--s2-erase_chip-bind-the-raw-rom-symbol)) — the "gap" is an alias omission, not a missing routine.** Verified against IDF release/v5.2 (`72d06017df`): (1) S2's `esp32s2.rom.spiflash_legacy.ld` aliases the *entire* legacy family to the original ROM names (`SPIRead`, `SPIWrite`, `SPIEraseSector`, `SPIEraseBlock`, `SPIEraseArea`, `SPI_Wait_Idle`, ...) and omits only chip-erase; the routine itself is exported as `SPIEraseChip = 0x400170ec` (`esp32s2.rom.ld:612`). (2) Espressif never added the alias because *nothing in IDF calls* `esp_rom_spiflash_erase_chip` — zero call sites outside `components/esp_rom` itself; modern IDF erases through the app-compiled flash HAL, and S2's `rom/spi_flash.h` doesn't even declare the legacy name. (3) The compiled ROM patch (`components/esp_rom/patches/esp_rom_spiflash.c` — the source ESP32's `libesp_rom.a` is built from) defines `esp_rom_spiflash_erase_chip` **for ESP32 only**; its entire S2 section is a single function (`esp_rom_spiflash_write_disable`), so there is nothing to precompile for S2. Resolution: esp-rom-sys adds the missing alias itself (`esp_rom_spiflash_erase_chip = SPIEraseChip`); the ESP32 patched implementation (wait-idle → CE → wait-idle) is the semantics reference for the one-off S2 hardware check. Re-verify against IDF master at implementation time in case a later IDF adds the alias upstream.

### B2 — read_user_cmd availability and shape

`esp_rom_spiflash_read_user_cmd(status: *mut u32, cmd: u8)` exists on **all 10 chips**: ESP32 native ROM `0x400621b0` (`esp32.rom.ld:1371`); S2 as an ld alias of `SPI_user_command_read` (= `0x40015fc8`, `esp32s2.rom.spiflash_legacy.ld:18`); P4 `0x4fc0016c`; all C-series and S3 in their respective ROM ld files. Per the IDF header it sends an arbitrary 8-bit command and reads back the response — **no address phase, no dummy cycles, no write-data phase**. Sufficient for JEDEC ID (RDID 0x9F) — which is all this design uses it for, as a private detection primitive ([A13](#a13--no-chip-configuration-at-launch)). The user-facing custom-command surface it once backed is shelved in [FLASH-DEFERRED.md](FLASH-DEFERRED.md).

### B3 — common_command and the host context

Moved to [FLASH-DEFERRED.md](FLASH-DEFERRED.md) ([A13](#a13--no-chip-configuration-at-launch): no consumer in this design). Heading retained for anchor stability.

---

## Appendix C — esp-storage internals evidence

File-level evidence for the Baseline section, gathered against the referenced tree.

### C1 — ROM call sites

The actual ROM FFI calls live in `esp-storage/src/hardware.rs:8-41` (via `esp_rom_sys::rom::spiflash`); `ll.rs` is only the thin public unsafe wrapper. ESP32 links `esp-rom-sys/libs/esp32/libesp_rom.a` (compiled from IDF v5.3.1 `esp_rom_spiflash.c`, #3688); every other chip resolves ROM symbols via `esp-rom-sys/ld/<chip>/`. The only SPI1 register access in the crate is RDID at `hardware.rs:70-73`.

### C2 — Chunking and critical sections

Each ROM call is individually wrapped in `maybe_with_critical_section` (`hardware.rs:8-30`); erase chunks per sector/block (`nor_flash.rs:161-187`), write per sector/page (`nor_flash.rs:124-156`, `storage.rs:48-73`); the lock is released between chunks, so interrupts re-enable there. Cache disable/restore happens inside the ROM routines themselves.

### C3 — Locking and multicore

The critical section takes a dedicated `esp_sync::RawMutex` (`lib.rs:48-58`) — on multi-core chips a cross-core spinlock plus executing-core interrupt disable; it cannot stop the other core's XIP. `MultiCoreStrategy` (`common.rs:229-242`): `Error` (default, set in `new()`, `common.rs:97`), safe `AutoPark` (`common.rs:248`), `unsafe Ignore` (`common.rs:258`); pre/post hooks guard write/erase only — reads have no guard. AutoPark's mechanism is `CpuControl::park_core` — a hardware CPU stall via RTC `sw_cpu_stall`/`options0` (`esp-hal/src/soc/esp32/cpu_control.rs:16-36`) — freezing the other core wherever it is, with no safe-point wait. esp-storage parks *before* taking the flash lock (`common.rs:282-289`), the ordering the new driver inverts (lock-before-park).

### C4 — Encrypted access and MMU

The ROM primitive `esp_rom_spiflash_write_encrypted` requires 32-byte alignment of destination address *and* length (`ll.rs:78-80`, `esp-rom-sys/src/rom/spiflash.rs:36-38`). esp-storage's `write_encrypted` (`encrypted.rs:44-79`) wraps it in a whole-4096-byte-sector RMW: MMU-decrypted read → merge → **erase sector** → rewrite — hence the overwrite semantics and the `EncryptedNorFlashRegion` `WRITE_SIZE = SECTOR_SIZE` (4096, `esp-bootloader-esp-idf/src/partitions.rs:873`). It returns `NotSupported` when `flash_encryption()` is off (`encrypted.rs:51-54`). `read_encrypted` maps temporary flash MMU pages for decrypted reads (`mmu.rs:66-134`); P4 invalidates both L1 D-cache and L2 (`mmu.rs:168-179`, the "dual-map" variant). HIL coverage (`hil-test/src/bin/storage.rs:43-88`) runs on encryption-disabled boards and asserts encrypted == plaintext — no CI device has encryption eFuses burned, so a true encrypted round-trip is never exercised.

### C5 — Capacity detection

ESP32 reads `g_rom_flashchip.chip_size` (`hardware.rs:54-65`; symbol at `esp-rom-sys/ld/esp32/rom/esp32.rom.ld:93`); all other chips RDID via SPI1 registers and decode with the esptool-derived ID→capacity table (`hardware.rs:67-121`, source URL in a comment at `:79`). An unknown ID falls through to `_ => 0` (`hardware.rs:90,118`), after which `check_bounds` fails every op with `OutOfBounds` (`common.rs:116-122`).

### C6 — Buffers and READ_SIZE

Word-aligned buffers are passed straight to the ROM with no residency check (`common.rs:173-192`, `nor_flash.rs:129-137`). Unaligned/RMW paths use stack locals: `FlashWordBuffer` (4 B) and `FlashSectorBuffer` (4096 B, `buffer.rs:12-14`; used on the stack in `storage.rs:55` and the read paths). The `bytewise-read` feature is **default-off** (`Cargo.toml:59-65`), so default esp-storage has `READ_SIZE = 4` (`nor_flash.rs:10-16`); `MultiwriteNorFlash` is implemented (`nor_flash.rs:242`).

### C7 — External-backend prior art

Moved to [FLASH-DEFERRED.md](FLASH-DEFERRED.md) with the external driver ([A12](#a12--internal-only-scope-the-external-backend-splits-out)). Heading retained for anchor stability.

---

## Glossary

- **XIP** — execute-in-place: code runs directly from cache-mapped flash. Why flash ops must disable the cache and why the other core is a hazard during writes.
- **SPI0/SPI1** — the internal memory-SPI pair: SPI0 serves the cache (XIP), SPI1 carries out flash transactions (what the ROM routines drive). The general-purpose SPI2/SPI3 masters are unrelated to this driver.
- **RDID / JEDEC ID** — the 0x9F read-identification command; its 3-byte response identifies vendor/type/capacity.
- **`g_rom_flashchip`** — the *legacy* ROM data global describing the boot flash, populated by ROM boot from the image header. The `g_rom_` prefix marks ROM-populated data.
- **Parking / AutoPark** — halting the other core via hardware stall (RTC `sw_cpu_stall`) for the duration of a flash chunk, so its XIP cannot touch the flash mid-operation.
- **Bounce buffer** — the driver-owned 256 B internal-RAM buffer that stages data whenever a user buffer lives outside internal RAM (flash `.rodata` or PSRAM).
- **RMW** — read-modify-write: reading a whole sector, merging changes, erasing, rewriting. The encrypted write wrapper does this internally; the plain NOR API deliberately does not.
- **Chunk** — one bounded ROM call (a page program, sector/block erase, or bounded read); the unit between which interrupts re-enable, the other core unparks, and async yields.
- **P4 dual-map** — ESP32-P4 cache invalidation must cover both L1 D-cache and L2 for MMU-remapped flash reads.
