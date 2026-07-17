# `esp_hal::flash` — Flash Storage Driver

Driver for SPI NOR flash, replacing `esp-storage`. `FlashStorage<'d, Dm>` works with both internal boot flash (SPI0/SPI1) and external SPI flash chips (SPI2, and SPI3 where present). The constructor input — the `FLASH` peripheral singleton, or a user-configured `Spi` driver for external chips — is immediately consumed into a private backend enum, so the *only* public type parameter is the [`DriverMode`] (`Blocking`/`Async`), following the esp-hal convention that every applicable driver offers both blocking and async APIs.

The backend is chosen at runtime (internal vs SPI); the driver mode is a compile-time type-state. Chip capability is resolved **per operation**: each operation uses the best primitive the chip has, and only operations the chip truly cannot execute fail, with `Error::NotSupported` at the point of use (see the per-operation capability table). The async API is interrupt/DMA-driven on the external SPI backend; on the internal backend it runs the chunked blocking ROM ops, yielding only *between* chunks (XIP forbids suspending mid-ROM-call — the executor itself lives in the flash being written; between chunks the cache is live and yielding is safe).

`FlashChipConfig` is the data-driven chip override — it carries the command bytes, timings, and geometry `FlashStorage` uses to drive the chip. There is no chip-*driver* trait; the config *is* the driver. Genuine per-chip *behavior* (quad-enable, 32-bit addressing, erase-suspend) is the only thing that would later justify a trait — added in unstable space if and when it's actually needed.

Per DEVELOPER-GUIDELINES (API Surface, since #5854): **all of this lands unstable** (`unstable_driver!`), and the [stabilization target](#stabilization-target) is promoted deliberately once the API has settled.

---

## Baseline: what `esp-storage` already does

This design ports and reorganizes; it must not silently regress the current crate. Facts the rest of the document builds on:

- Read/write/erase go through ROM `esp_rom_spiflash_*` on **all 10 chips** (`esp-storage/src/ll.rs`). ESP32's incomplete ROM is patched by a static library in `esp-rom-sys` (`libs/esp32/libesp_rom.a`, #3688). There is **no** SPI1 register path for standard operations — only RDID capacity detection touches SPI1 registers (`hardware.rs`).
- The constructor already takes the `peripherals.FLASH` virtual singleton. It exists on all 10 chips in esp-metadata but is currently **unstable** (no `stable = true` in any `soc.toml`).
- Erase and write are already **chunked**: per-sector/block/page ROM calls, each inside its own critical section. Interrupts re-enable between chunks; the cache is disabled only inside each ROM call. Worst-case interrupts-off window ≈ one 64 KiB block erase.
- The critical section is **current-core only** (`esp_sync::RawMutex`). Cross-core safety is `MultiCoreStrategy` (`Error` default, `AutoPark`, unsafe `Ignore`) and guards write/erase only — **reads are unguarded today**.
- **Encrypted flash is load-bearing**: `read_encrypted`/`write_encrypted` exist, `esp-bootloader-esp-idf` routes every partition access through `is_effectively_encrypted()` (`FlashRegion::read/write`), and since #5857 exposes `NorFlashRegion` / `EncryptedNorFlashRegion` wrapper views (`as_nor_flash()` / `as_nor_flash_encrypted()`), the encrypted one with `WRITE_SIZE = 4096` because the ROM encrypts whole sectors. `hil-test/src/bin/storage.rs` covers the encrypted path on hardware.
- **MMU internals** (`esp-storage/src/mmu.rs`) exist for encrypted reads and cache invalidation, including the P4 dual-map variant.
- `pub mod ll` has external consumers: `hil-test/src/bin/alloc_psram.rs` calls `esp_storage::ll::spiflash_write`.
- An `emulation` feature (esp-hal is an *optional* dep of esp-storage) swaps `hardware.rs` for `stub.rs`; **esp-bootloader-esp-idf's host tests run on it** via `cargo xtask host-tests`.
- Capacity detection: ESP32 reads `g_rom_flashchip.chip_size` (RDID unreliable there); all others RDID via SPI1 registers. An unknown ID yields capacity **0**, after which every operation fails `OutOfBounds` with no indication why.
- `esp-storage/build.rs` rejects opt-level 0/1 on ESP32 — a timing-related requirement predating #3688, from when the flash routines were compiled from Rust. **Dropped** for the new driver (resolved question 2): the timing-sensitive code is now precompiled in the ESP32 static ROM lib and the remaining Rust is thin `#[ram]` wrappers, identical in shape to the other nine chips (which never had the requirement).
- Word-aligned write sources are passed straight to the ROM with no flash-residency check — writing a `const` array from `.rodata` reads flash while the cache is off. Latent footgun; this design closes it (see write path).

---

## Module structure

```
esp_hal::flash
├── mod.rs               — public types, re-exports
├── storage.rs           — FlashStorage<'d, Dm> (backend-erased, mode type-state)
└── host.rs              — FlashBackend (ops-shaped), FlashCommand lowering (ALL PRIVATE)
```

Public items in `mod.rs`:
- `Config`, `ConfigError`
- `Error`
- `FlashChipConfig`, `FlashChipInfo`
- `MultiCoreStrategy` (multi-core chips, unstable)
- `MappedFlash` (unstable, returned by the inherent `mmap`)
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
//! Driver for SPI NOR flash chips. Works with both the internal boot
//! flash (SPI0/SPI1) and external SPI flash chips (SPI2/SPI3).
//! {documentation}
//!
//! ## Configuration
//! The default [`Config`] auto-detects flash geometry from ROM/RDID.
//! For non-standard chips, set a [`FlashChipConfig`] on [`Config`] via
//! `with_chip` to override geometry, commands, and timing.
//!
//! ## Usage
//! With the `unstable` feature, implements
//! [`embedded_storage::nor_flash::ReadNorFlash`], [`NorFlash`] and
//! [`MultiwriteNorFlash`] (async equivalents on `FlashStorage<'_, Async>`).
//! The legacy `ReadStorage`/`Storage` traits are intentionally not
//! implemented — use explicit `erase` then `write`.
//!
//! ## Examples
//! (short read example using {before_snippet}/{after_snippet})
//!
//! ## Implementation State
//! - Blocking API — first stabilization candidate
//! - Async API (via [`into_async`]) — interrupt/DMA-driven on the external
//!   SPI backend; chunked blocking ROM ops with inter-chunk yields on the
//!   internal backend
//! - Encrypted read/write (flash encryption) — internal backend only
//! - Memory-mapped reads — via the inherent [`mmap`] method
//! - External SPI flash — new capability, no esp-storage precedent
//! - Dual/Quad SPI IO modes — not yet supported
//!
//! ## Limitations
//! - During each ROM chunk, interrupts are disabled on the executing core;
//!   handlers that must run during flash operations (and everything they
//!   touch) must live in RAM and must not access flash or PSRAM.
//! - On multi-core chips, `write`/`erase` park the other core for the
//!   duration of each chunk (default `MultiCoreStrategy`) — anything running
//!   there (including esp-radio) stalls briefly. Unstable strategies opt out.
//! - Custom `FlashChipConfig` commands needing address/dummy/data phases
//!   (read, program, erase) are not executable on the internal flash of
//!   ESP32/S2/P4 — those operations return [`Error::NotSupported`]. Use the
//!   external backend for such chips.
//! - An external flash chip owns its SPI bus; sharing the bus with other
//!   devices is not supported (consequence of the backend-erased design).
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

    /// Override auto-detected chip parameters. `None` = auto-detect
    /// from ROM/RDID. Applies to both `new` (internal) and `new_spi`
    /// (external). Geometry/command/timing setters are unstable; the
    /// `capacity` override is a stabilization candidate (see below).
    chip: Option<FlashChipConfig>,
}
```

On multi-core chips the default strategy is **`AutoPark`** (resolved 2026-07-16): stable `write`/`erase` park the other core for the duration of each chunked ROM call and unpark it between chunks. This is the documented stable contract — "write/erase briefly halt the other core" — because the alternatives don't hold up: `Error` as default would leave stable-only users unable to write flash at all (strategy selection is unstable), and a "blocking critical section" is illusory — a non-cooperating core executing XIP can only be kept off the flash by parking it. `MultiCoreStrategy` (`Error`, and the `unsafe` `Ignore`) stays an **unstable** opt-in. Migration note required: esp-storage's default is `Error`/`OtherCoreRunning`.

**Reads** are not multicore-guarded (matching esp-storage). Expected sound — SPI0/SPI1 arbitrate in hardware, so the other core's cache refills interleave with SPI1 read transactions — but the claim is verified by a dedicated dual-core HIL case (core 1 in an XIP hot loop, core 0 hammering `read`) and the outcome recorded in stable `read`'s documentation.

### `ConfigError`

Auto-detection is *not* infallible (unknown RDID ⇒ capacity 0 today), and a user-supplied `FlashChipConfig` can be nonsense — per the guidelines, an empty `ConfigError` is therefore wrong here:

```rust
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum ConfigError {
    /// Flash geometry could not be detected and no override was provided.
    /// (Replaces esp-storage's silent capacity-0 behavior.)
    DetectionFailed,
    /// The provided `FlashChipConfig` is invalid (zero capacity or page
    /// size, capacity not a multiple of the sector size, ...).
    InvalidChipConfig,
}
// No capability variant: chip capability is per-operation, surfaced at the
// point of use as `Error::NotSupported` (see per-operation
// capability table). `apply_config` validates values, not capability.

impl core::fmt::Display for ConfigError { ... }
impl core::error::Error for ConfigError {}
```

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

Resolved (2026-07-16): module-scoped `flash::Error` (esp-storage's `FlashStorageError` name is not carried over), `Unknown` instead of `Other` per esp-hal convention, no `i32` payload (an unexpected ROM return code is a bug to report, not data to branch on). SPI-backend failure variants (e.g. DMA errors) are added together with the `Spi` backend — `#[non_exhaustive]` makes that additive. A `NorFlashError`/`NorFlashErrorKind` mapping is required by the traits (`NotAligned`/`OutOfBounds` map directly, rest `Other`), as in `esp-storage/src/nor_flash.rs` today.

### `FlashChipConfig`

```rust
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, BuilderLite)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct FlashChipConfig {
    capacity: u32,
    #[builder_lite(unstable)]
    sector_size: u32,
    #[builder_lite(unstable)]
    block_size: u32,
    #[builder_lite(unstable)]
    page_size: u32,

    #[builder_lite(unstable)]
    read_command: u8,
    #[builder_lite(unstable)]
    page_program_command: u8,
    #[builder_lite(unstable)]
    sector_erase_command: u8,
    #[builder_lite(unstable)]
    block_erase_command: u8,
    #[builder_lite(unstable)]
    chip_erase_command: u8,
    #[builder_lite(unstable)]
    write_enable_command: u8,
    #[builder_lite(unstable)]
    read_status_command: u8,
    #[builder_lite(unstable)]
    status_busy_mask: u8,

    #[builder_lite(unstable)]
    page_program_timeout_us: u32,
    #[builder_lite(unstable)]
    sector_erase_timeout_us: u32,
    #[builder_lite(unstable)]
    block_erase_timeout_us: u32,
    #[builder_lite(unstable)]
    chip_erase_timeout_us: u32,
}
```

`Default` is **hand-written**, not derived (a derived default would be all-zero commands and zero geometry — guaranteed nonsense). Default = the standard 25-series command set (read 0x03, page-program 0x02, sector-erase 0x20, block-erase 0xD8, chip-erase 0xC7, write-enable 0x06, read-status 0x05, busy mask 0x01), 256 B page, 4 KiB sector, 64 KiB block, ESP-IDF-derived timeouts. `capacity` has no meaningful default; `apply_config` rejects invalid values with `ConfigError::InvalidChipConfig`.

### `FlashChipInfo`

```rust
#[instability::unstable]
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct FlashChipInfo {
    pub chip_id: u32,
    pub capacity: u32,
    pub sector_size: u32,
    pub block_size: u32,
    pub page_size: u32,
}
```

---

## Custom chips (unstable) and backend selection

For a chip whose command bytes, timing, or geometry differ from the auto-detected defaults, set a [`FlashChipConfig`] on the driver [`Config`] via `Config::with_chip(...)` (`None` = auto-detect). It applies to both `new` (internal) and `new_spi` (external) — no per-constructor variants; runtime changes go through the existing `apply_config`. `FlashChipConfig` *is* the data-driven driver. There is deliberately no chip-driver trait: it would carry only data and no behavior. `FlashCommand` is private in `host.rs`; all SPI execution stays internal.

**Per-operation capability** (resolved 2026-07-16 — the API is designed around user operations, not around ROM's shape; each operation uses the best primitive the chip has, and only the operations the chip truly cannot execute fail):

| Operation, as configured | Internal backend | `Spi` backend |
|--------------------------|------------------|---------------|
| default commands (incl. `capacity`-only override — the stabilization-candidate use case) | dedicated `esp_rom_spiflash_*` — all 10 chips | SPI master |
| custom command, command+response shape (status reads, JEDEC ID) | `esp_rom_spiflash_read_user_cmd` — all 10 chips | SPI master |
| custom command needing address/dummy/data phases (read, program, erase), or custom geometry | `spi_flash_hal_common_command` — S3/C2/C3/C5/C6/C61/H2; **`Error::NotSupported` on ESP32/S2/P4** | SPI master |

Capability errors surface **per operation, at the point of use** — not at config time. `apply_config` validates *values* (`InvalidChipConfig`), never chip capability: a config is legitimate if the operations you actually use are executable. Example: a custom busy-poll/status command works on every chip, including ESP32 — rejecting the whole config because its (never-called) custom erase command can't run there would throw that away.

**ROM command capability, verified against the ROM linker scripts and IDF sources (2026-07-16):**

- `esp_rom_spiflash_*` (the dedicated per-operation functions, including `erase_chip`): **all 10 chips**, so the default and capacity-override paths work everywhere.
- `esp_rom_spiflash_read_user_cmd(status: *mut u32, cmd: u8)`: **all 10 chips** (ESP32 native ROM `0x400621b0`; S2 alias of `SPI_user_command_read`; P4 `0x4fc0016c`; C-series/S3). Per the IDF header it sends an arbitrary 8-bit command and reads back the response — **no address phase, no dummy cycles, no write-data phase**. Sufficient for JEDEC ID and custom status reads; insufficient for custom erase (needs address) or program (needs address + data).
- `spi_flash_hal_common_command` + its `esp_flash_default_chip` host context: **exactly S3/C2/C3/C5/C6/C61/H2** (verified against `esp-rom-sys/ld/`; absent on ESP32/S2/P4). Executes a full `spi_flash_trans_t` — 8/16-bit command, address + bit length, dummy cycles, mosi and miso data (verified against IDF `components/hal/spi_flash_hal_common.inc`). Its absence is the precise reason full `FlashChipConfig` overrides are `NotSupported` on ESP32/S2/P4.

**Detection / `chip_info` story — no register code anywhere:** the JEDEC ID comes from `esp_rom_spiflash_read_user_cmd(0x9F)` on all chips except ESP32, which reads the ROM data global `g_rom_flashchip` (`device_id`, `chip_size`) because hardware RDID is unreliable there. This replaces even esp-storage's one remaining register touch (the SPI1 `flash_rdid` dedicated command bit); capacity decode keeps the esptool table.

**Resolved (2026-07-16): no SPI1 register fallback for ESP32/S2.** The custom-chip user on those targets is driving an external data chip ("EEPROM" use case) on SPI2/3 — the `Spi` backend handles arbitrary commands there with no register code and no XIP hazard. Boot flash on ESP32/S2 is a standard chip covered by the ROM path. Per-op `NotSupported` → supported later is additive, so a register path can still be added if a genuine boot-flash-with-custom-commands user ever appears.

Timeout overrides only apply on command-capable backends — the plain ROM functions own their timing. Auto-detection (JEDEC ID → known config) is internal: a private table, not a user-implementable hook. The override path already lets the user state their chip explicitly.

Overriding `sector_size`/`block_size`/`page_size` does **not** change the `embedded-storage` `WRITE_SIZE`/`ERASE_SIZE` consts — those stay fixed at the ESP hardware values (4 / 4096). A chip with non-standard geometry must use the inherent `erase()`/`write()` and not rely on the const-based `NorFlash` trait.

### Future: a behavioral trait

A trait becomes justified only once a chip needs genuine *behavior* that `FlashChipConfig` can't encode as data — quad-enable procedures, 32-bit addressing for >16 MB parts, erase suspend/resume, custom protection/OTP sequences (cf. ESP-IDF's `spi_flash_chip_t` vtable). All future and unstable: introduce the trait then, with the signatures the real need reveals — not speculatively now.

---

## `FlashStorage<'d, Dm>`

The driver carries the esp-hal [`DriverMode`] type-state. Constructors always build a `Blocking` driver; `into_async` / `into_blocking` flip between modes. Because esp-hal mode parameters carry **no default** (`I2c<'d, Dm: DriverMode>`, `Spi<'d, Dm: DriverMode>`), the `Dm` parameter must be committed on day one — adding it after stabilization is a breaking change to every `FlashStorage` annotation. The async *implementation* is promoted later (non-breaking). `Async` is `!Send` by construction (`Async(PhantomData<*const ()>)`, `lib.rs:556` — the `PhantomData<Dm>` field propagates it); `Blocking` is `Send`.

```rust
#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct FlashStorage<'d, Dm: DriverMode> {
    backend: FlashBackend,           // private, runtime-erased (internal vs SPI)
    capacity: u32,
    unlocked: bool,
    _mode: PhantomData<Dm>,          // Dm = Async ⇒ driver is !Send (pinned to init core)
    _lifetime: PhantomData<&'d ()>,
}
```

No `PeripheralGuard` field: `system::Peripheral` has no `Flash` variant (FLASH is virtual with no clock gate — absent from every `clocks.toml`), and the internal backend needs no Drop work. The SPI backend stores the user's `Spi` driver, which brings its own guard and handles Drop per the guidelines.

```rust
// Constructors always produce a Blocking driver (esp-hal convention).
impl<'d> FlashStorage<'d, Blocking> {
    pub fn new(
        flash: FLASH<'d>,
        config: Config,
    ) -> Result<Self, ConfigError>;   // DetectionFailed surfaces here, not as capacity-0

    #[instability::unstable]
    pub fn new_spi(
        spi: Spi<'d, Blocking>,
        config: Config,
    ) -> Result<Self, ConfigError>;
    // The SPI driver arrives fully configured — pins including hardware CS,
    // mode, frequency — and is consumed into the private backend. FlashStorage
    // does not route pins or own bus setup. DMA/async plumbing for this
    // backend is settled when the (unstable) backend lands.

    /// Convert into an async driver. Interrupt/DMA-driven on the external SPI
    /// backend; the internal backend runs chunked blocking ROM ops and yields
    /// between chunks.
    #[instability::unstable]
    pub fn into_async(self) -> FlashStorage<'d, Async>;
}

#[instability::unstable]
impl<'d> FlashStorage<'d, Async> {
    pub fn into_blocking(self) -> FlashStorage<'d, Blocking>;
    // async ops are exposed via the embedded_storage_async impls below
}

// Operations work in any mode — blocking calls remain available on an async
// driver (see esp_hal `DriverMode` docs: cheaper for small transfers).
impl<'d, Dm: DriverMode> FlashStorage<'d, Dm> {
    pub fn apply_config(&mut self, config: &Config) -> Result<(), ConfigError>;

    pub fn read(&mut self, offset: u32, buffer: &mut [u8]) -> Result<(), Error>;
    pub fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Error>;
    pub fn erase(&mut self, from: u32, to: u32) -> Result<(), Error>;

    /// Erases the entire chip. Capability-honest on both backends (the ROM
    /// provides `esp_rom_spiflash_erase_chip` everywhere); no runtime nannying.
    ///
    /// On the internal backend, understand what you are asking for:
    /// - This is a single, un-chunkable ROM call: multi-second, interrupts
    ///   disabled on the executing core for the entire duration. The async
    ///   version cannot yield — it is one giant blocking call in any mode.
    /// - Under the default `MultiCoreStrategy` the other core is parked for
    ///   the full duration: both cores are dead for seconds.
    /// - On return, the running program's own text is gone — the next cache
    ///   miss fetches erased flash. Only coherent from fully IRAM-resident
    ///   code (flasher-stub-style factory reset).
    #[instability::unstable]
    pub fn erase_chip(&mut self) -> Result<(), Error>;

    pub fn capacity(&self) -> usize;
    #[instability::unstable]
    pub fn chip_info(&self) -> FlashChipInfo;

    // Flash-encryption aware access (internal backend only; `NotSupported`
    // on the SPI backend). Ports esp-storage's encrypted.rs + mmu.rs.
    // Load-bearing for esp-bootloader-esp-idf: `FlashRegion::read/write`
    // dispatch on `is_effectively_encrypted()`, and `EncryptedNorFlashRegion`
    // (WRITE_SIZE = 4096 — the ROM encrypts whole sectors) delegates here.
    #[instability::unstable]
    pub fn read_encrypted(&mut self, offset: u32, buffer: &mut [u8]) -> Result<(), Error>;
    #[instability::unstable]
    pub fn write_encrypted(&mut self, offset: u32, data: &[u8]) -> Result<(), Error>;
}
```

**Write path**: sources residing in the flash address window are bounced through the RAM sector buffer (fixes the esp-storage footgun where a word-aligned `.rodata` source is read from flash while the cache is off). Unaligned sources are buffered as today.

### `mmap` (unstable)

```rust
impl<'d, Dm: DriverMode> FlashStorage<'d, Dm> {
    /// Memory-map a region of the internal flash for zero-copy reads.
    /// `NotSupported` on the SPI backend — per-op capability, like every
    /// other operation.
    #[instability::unstable]
    pub fn mmap(&mut self, offset: u32, len: u32) -> Result<MappedFlash<'_>, Error>;
}
```

Resolved (2026-07-16): this was an `InternalFlashExt` extension trait; dropped — single implementor, no compile-time backend separation after erasure, and per-op `NotSupported` is the design's uniform capability model. `MappedFlash` borrows `&mut self`, so no write/erase while mapped.

```rust
// Blocking embedded-storage traits — available in any mode. All trait impls
// are gated behind the `unstable` feature (see dependency policy below).
// Geometry consts are FIXED at the ESP hardware values and do NOT reflect any
// runtime chip-config geometry override.
impl<Dm: DriverMode> ReadNorFlash for FlashStorage<'_, Dm> {
    const READ_SIZE: usize = 1; // bytewise — unaligned reads buffered, aligned reads fast-path
                                // (replaces esp-storage's `bytewise-read` feature)
}
impl<Dm: DriverMode> NorFlash for FlashStorage<'_, Dm> {
    const WRITE_SIZE: usize = 4;    // word-aligned (ROM requirement)
    const ERASE_SIZE: usize = 4096; // sector — fixed across all ESP flash
}
impl<Dm: DriverMode> MultiwriteNorFlash for FlashStorage<'_, Dm> {}
// MultiwriteNorFlash is only valid for non-encrypted access (doc note; the
// encrypted path never gets a Multiwrite impl).

// The legacy `ReadStorage`/`Storage` (RMW) family is intentionally NOT
// implemented — the NOR family is the modern surface. Callers needing
// sub-sector updates own the read-modify-write.

// Async embedded-storage traits — Async mode only.
impl embedded_storage_async::nor_flash::ReadNorFlash for FlashStorage<'_, Async> { ... }
impl embedded_storage_async::nor_flash::NorFlash for FlashStorage<'_, Async> { ... }
impl embedded_storage_async::nor_flash::MultiwriteNorFlash for FlashStorage<'_, Async> {}
```

**Dependency/feature policy** — no new cargo features. esp-hal's pattern for pre-1.0 ecosystem crates is version-suffixed optional deps enabled by `unstable` (`embedded-io-06`, `embedded-io-07`, `embedded-can`, ...; only 1.0 crates like embedded-hal are unconditional). So: deps land as `embedded-storage-03` / `embedded-storage-async-04` (renamed), pulled in by `unstable`; all trait impls are `#[instability::unstable]`. Whether the impls later stabilize on the 0.3 dep (deliberate policy exception) or wait for embedded-storage 1.0 is **deferred to the stabilization PR** (resolved 2026-07-16) — the suffix pattern keeps either answer additive, so nothing is gained by committing early. The inherent `read`/`write`/`erase`/`capacity` API is the stable surface; this mirrors UART (stable inherent API, unstable embedded-io impls).


---

## Private internals (`host.rs`)

The backend interface is **ops-shaped** — designed around driver operations, not around ROM entry points. Every method is total on every variant: each operation resolves to the best available primitive for the configured command, or `Err(NotSupported)`. There is no `exec`-style dispatch that is only valid on some variants, so no `unreachable!()` and no panic path.

```rust
enum FlashBackend {
    Internal(InternalState),  // per-op capability resolution — no config-time split
    Spi(SpiState),            // SPI2/3 master driver — all commands supported
}

struct InternalState {
    // esp_flash_default_chip->host, for spi_flash_hal_common_command.
    // Exists only on S3/C2/C3/C5/C6/C61/H2; initialized by the 2nd-stage
    // bootloader's flash init — validate before use.
    #[cfg(any(esp32s3, esp32c2, esp32c3, esp32c5, esp32c6, esp32c61, esp32h2))]
    rom_ctx: *mut core::ffi::c_void,
}

struct SpiState { /* the user-configured Spi driver — brings its own PeripheralGuard */ }

impl FlashBackend {
    fn read(&mut self, chip: &FlashChipConfig, offset: u32, buf: &mut [u8]) -> Result<(), Error>;
    fn write_page(&mut self, chip: &FlashChipConfig, offset: u32, data: &[u8]) -> Result<(), Error>;
    fn erase_sector(&mut self, chip: &FlashChipConfig, index: u32) -> Result<(), Error>;
    fn erase_block(&mut self, chip: &FlashChipConfig, index: u32) -> Result<(), Error>;
    fn erase_chip(&mut self, chip: &FlashChipConfig) -> Result<(), Error>;
    fn read_status(&mut self, chip: &FlashChipConfig) -> Result<u8, Error>;
    fn write_enable(&mut self, chip: &FlashChipConfig) -> Result<(), Error>;
    fn read_id(&mut self) -> Result<u32, Error>;

    fn is_internal(&self) -> bool {
        matches!(self, FlashBackend::Internal(_))
    }
}
```

Per-operation resolution on `Internal`, in order:

1. operation uses its default command → dedicated `esp_rom_spiflash_*` function (all 10 chips)
2. custom command with a command+response shape (status reads, ID) → `esp_rom_spiflash_read_user_cmd` (all 10 chips)
3. custom command needing address/dummy/data phases (read, program, erase), or custom geometry the dedicated ROM functions can't honor → `spi_flash_hal_common_command` (S3/C2/C3/C5/C6/C61/H2), else `Err(NotSupported)`

On `Spi`, every operation lowers to a `FlashCommand` and executes as a half-duplex master transaction. `FlashCommand` stays private as the shared lowering used by `rom_hal_exec` and `spi_master_exec`; it is an implementation detail of steps 2–3, not the backend interface.

`spi_master_exec` builds on the existing SPI master half-duplex API (`Command`/`Address` phases + DMA + hardware CS). Prior art: `qa-test/src/bin/qspi_flash.rs` already drives a GD25Q64C on SPI2 exactly this way with raw command bytes and fixed `delay_millis(250)` waits — the `Spi` backend is that pattern productized (RDSR busy-polling with the `FlashChipConfig` busy mask and timeouts instead of fixed delays). Note the test's chip filter `spi_master_supports_dma && !esp32p4`: the DMA-driven async path inherits the P4 SPI-DMA gap until that lands.

Async operations reuse the same `FlashBackend`: the `Spi` backend starts a DMA transfer and awaits completion via interrupt; the internal backend runs each chunked ROM call blocking (per page/sector/block, exactly the chunking esp-storage already does) and **yields between chunks**, bounding executor stall to one ROM call. Switching to `Async` sets up the SPI interrupt handler only for the `Spi` backend; for the internal backend `into_async` only changes the type-state. Per the `DriverMode` contract, `Async` is `!Send` (handler pinned to the init core); `Blocking` is `Send`. This is orthogonal to `MultiCoreStrategy`, which governs the *other* core executing from flash during a write.

There is deliberately **no SPI1 register-exec path** (resolved — see backend selection rules): internal operations that would need it return `Error::NotSupported` on ESP32/S2/P4, which keeps RAM-resident register bit-banging out of the design entirely.

New ROM bindings land in `esp-rom-sys/src/rom/spiflash.rs`: `esp_rom_spiflash_read_user_cmd` (all chips) and `spi_flash_hal_common_command` + `esp_flash_default_chip` (7 chips) — chip coverage as verified in the capability inventory above.

---

## Host tests / emulation

esp-bootloader-esp-idf's host tests (and `cargo xtask host-tests`) currently run against esp-storage's `emulation` feature — possible only because esp-hal is an *optional* dependency of esp-storage. esp-hal cannot build for the host, so the new driver cannot carry an emulation backend. The bootloader must decouple its tests from the concrete flash type (internal test mock behind `cfg(test)`, or a small internal abstraction over its `FlashRegion` accesses). This is a prerequisite for the migration, not an afterthought — without it, deprecating esp-storage breaks the bootloader's test suite.

---

## Stabilization target

Everything lands unstable. This table is the *target* stable surface for the eventual, deliberate stabilization PR (semver baseline update included):

| Item | Stable target | Stays unstable |
|------|---------------|----------------|
| `Config`, `ConfigError` | yes | |
| `FlashStorage<'d, Dm: DriverMode>` — the mode parameter¹ | yes | |
| `FlashStorage::new()` → `FlashStorage<'d, Blocking>` | yes | |
| `FlashStorage::apply_config()` | yes | |
| `FlashStorage::read/write/erase/capacity` | yes | |
| `Config::with_chip()` + `FlashChipConfig` capacity override² | yes | |
| `FlashChipConfig` geometry / command / timing setters | | yes |
| `Error` | yes | |
| `peripherals.FLASH` singleton (currently unstable) | yes (flip `stable = true` in 10 `soc.toml`s + `update-metadata`) | |
| `FlashStorage::erase_chip()` | | yes |
| `FlashStorage::chip_info()`, `FlashChipInfo` | | yes |
| `read_encrypted` / `write_encrypted` | | yes |
| `ReadNorFlash`/`NorFlash`/`MultiwriteNorFlash` impls | | yes — stabilization decision deferred to the stabilization PR |
| `FlashStorage::new_spi()` | | yes |
| `into_async()` / `into_blocking()` | | yes |
| `embedded-storage-async` trait impls (`Async` mode) | | yes |
| `mmap()`, `MappedFlash` | | yes |
| `MultiCoreStrategy`, `OtherCoreRunning` (multi-core) | | yes |

¹ The `Dm` type parameter must ship on day one: esp-hal mode parameters carry **no default**, so adding it later breaks every `FlashStorage` annotation. Only the async *implementation* is deferred — promoting it later is non-breaking.

² Paired deliberately: a stable capacity setter is useless if `with_chip` stays unstable (that was the only route to it). Capacity override is the answer to detection failure (`ConfigError::DetectionFailed`), which is a stable-path concern.

---

## Files to create/modify

| File | Action |
|------|--------|
| `esp-hal/src/flash/mod.rs` | **Create** — public types, re-exports |
| `esp-hal/src/flash/storage.rs` | **Create** — `FlashStorage` |
| `esp-hal/src/flash/host.rs` | **Create** — private ops-shaped backend |
| `esp-hal/src/lib.rs` | Add flash module via `unstable_driver!` |
| `esp-hal/Cargo.toml` | Add optional `embedded-storage-03` / `embedded-storage-async-04` deps to the `unstable` feature (no new features) |
| `esp-metadata/devices/*/soc.toml` (×10) | Add `[device.flash]` driver entry (`flash_driver_supported` symbol, README matrix row); at stabilization: `stable = true` on FLASH |
| `esp-metadata-generated/` | `cargo xtask update-metadata` |
| `esp-rom-sys/src/rom/spiflash.rs` | Add `spi_flash_hal_common_command` / `esp_flash_default_chip` bindings (7 chips) + `esp_rom_spiflash_read_user_cmd` (all chips — JEDEC ID / `chip_info`) |
| `esp-bootloader-esp-idf/` | Swap concrete flash type (decide: `FlashStorage<'d, Blocking>` vs generic `Dm`); own the RMW inside `FlashRegion::write` (byte-granular public API today, backed by esp-storage's `Storage` RMW); keep `NorFlashRegion`/`EncryptedNorFlashRegion` (#5857) working via the new encrypted API; decouple host tests from esp-storage emulation; drop or reimplement `ReadStorage`/`Storage` impls on `FlashRegion` (breaking — migration guide) |
| `examples/peripheral/flash_read_write/` | Migrate to `esp_hal::flash` |
| `examples/ota/update/` | Migrate (writes app image via `FlashRegion::write`) |
| `hil-test/src/bin/storage.rs` | Migrate (covers encrypted path); keep HIL coverage |
| `hil-test/src/bin/alloc_psram.rs` | Migrate `esp_storage::ll::spiflash_write` to `FlashStorage::write` (it only needs a flash write to happen while PSRAM is live) |
| `qa-test/src/bin/multicore_flash.rs` | Migrate `multicore_auto_park`/`multicore_ignore` to `Config` strategy |
| `qa-test/src/bin/qspi_flash.rs` | Migrate raw half-duplex commands + fixed delays to `FlashStorage::new_spi` (becomes the external-backend qa test) |
| `hil-test/Cargo.toml`, `qa-test/Cargo.toml` | Feature wiring |
| `esp-storage/` | Deprecation notice (first crate deprecation in the workspace — mechanism TBD: README + crates.io description + doc banner) |

Changelog and migration-guide entries go in the **PR description** (structured sections), not `CHANGELOG.md` (per CONTRIBUTING.md).

---

## Verification

1. `cargo xtask lint-packages --packages esp-hal --chips esp32c6` (ROM-HAL path)
2. `cargo xtask lint-packages --packages esp-hal --chips esp32s3` (Xtensa + multi-core)
3. `cargo xtask lint-packages --packages esp-hal --chips esp32` (static ROM lib)
4. `cargo xtask lint-packages --packages esp-hal --chips esp32p4` (no ROM HAL — `NotSupported` path)
5. `cargo xtask fmt-packages`
6. `cargo xtask run doc-tests <CHIP>` + `cargo xtask build documentation`
7. Build flash examples and `esp-bootloader-esp-idf`
8. `cargo xtask host-tests` — requires the bootloader emulation decoupling above
9. HIL: migrated `storage` test (including encrypted read/write round-trip)
10. HIL: `storage` test on ESP32 built at opt-level 0 — confirms dropping the esp-storage opt-level requirement (resolved question 2)
11. HIL: OTA slot-switch round-trip — verifies the `otadata` update (explicit erase + write, guarding against sub-sector corruption/bricking after dropping the `Storage` RMW family)
12. qa-test: `multicore_flash` on S3
13. HIL: dual-core read soundness — core 1 in an XIP hot loop while core 0 hammers `read` (resolved question 4)

---

## Resolved questions

1. **ESP32/S2 register-exec path — removed** (2026-07-16). The custom-chip user on ESP32/S2 drives an external data chip on SPI2/3 (the `Spi` backend — prior art `qa-test/src/bin/qspi_flash.rs`); boot flash there is a standard chip on the ROM path. Internal operations that would need it return per-op `Error::NotSupported` on ESP32/S2/P4 — additive to lift later if a real boot-flash user appears.
2. **ESP32 opt-level ≥ 2 requirement — dropped** (2026-07-16). It was timing-related and predates #3688; the timing-sensitive code is precompiled in the ESP32 static ROM lib, and the remaining thin `#[ram]` ROM wrappers match the other chips, which never had the requirement. The new driver carries no build.rs check. Confirmation gate: run the migrated `storage` HIL test on ESP32 in a debug build before landing.
3. **`exec` + `unreachable!()` — dissolved** (2026-07-16). The backend interface is ops-shaped, designed around user operations rather than ROM entry points: each operation resolves per chip to a dedicated ROM function, `esp_rom_spiflash_read_user_cmd` (command+response, all chips), `spi_flash_hal_common_command` (full transactions, 7 chips), or the SPI master — and returns `Error::NotSupported` only for what the chip truly cannot execute. No dispatch method is partial over the backend enum, so no panic arm exists. `FlashCommand` remains as private lowering shared by the two command-capable paths.
4. **Multi-core stable default — `AutoPark`** (2026-07-16). `Error` is unshippable as a stable default (strategy selection is unstable, so stable users would have no recourse), and a blocking cross-core critical section cannot exist for non-cooperating XIP code. Stable contract: write/erase briefly halt the other core, documented. `Error`/`Ignore` stay unstable opt-ins; migration note vs esp-storage's `Error` default. Reads stay unguarded, backed by a dual-core HIL verdict recorded in stable `read` docs.
5. **`InternalFlashExt` — dropped** (2026-07-16). `mmap` is an inherent unstable method returning `NotSupported` on the SPI backend, consistent with the per-op capability model. `MappedFlash` stays.
6. **Error type shape** (2026-07-16). `flash::Error` (module-scoped, per convention), `Unknown` not `Other`, no payload, `#[non_exhaustive]`; SPI-backend error variants added when the `Spi` backend lands.
7. **`erase_chip` on the internal backend — allowed, unstable, loud docs** (2026-07-16). `NotSupported` would violate the per-op capability law (the chip can execute it), and `unsafe` would be incoherent (safe `erase` can already wipe the running app). Doc block states: single un-chunkable multi-second ROM call with interrupts off; other core parked the whole time under `AutoPark`; the caller's own text is erased on return — only coherent from IRAM-resident code.
8. **`new_spi` shape — takes a pre-configured `Spi<'d, Blocking>`** (2026-07-16). The user owns bus setup (pins including hardware CS, mode, frequency) before passing the driver in; `FlashStorage` consumes it into the private backend, so no new public type parameters — erasure intact. DMA/async plumbing for this backend is an implementation detail settled when the unstable backend lands.
9. **embedded-storage trait stabilization — deferred** (2026-07-16). Impls stay `#[instability::unstable]`; the 1.0-policy-vs-exception call belongs to the stabilization PR, with usage data in hand. The version-suffix dep pattern (`embedded-storage-03`) keeps either outcome additive.
10. **`ll` module — private** (2026-07-16). No public `esp_hal::flash::ll`: it would bypass bounds checks, unlock state, and `MultiCoreStrategy` under esp-hal's name, and the only public-`ll` precedent (`clock::ll`) exists because there is no full driver there. `esp-rom-sys` is the designated raw escape hatch; `alloc_psram.rs` migrates to `FlashStorage::write`. Thin ROM wrappers live on as a private module; making them public later is additive if a real user appears.

## Open questions

None — all questions raised in review are resolved above (2026-07-16).
