# `esp_hal::flash`: Internal flash driver

This document defines the driver for the internal boot flash on SPI0/SPI1. The
driver replaces `esp-storage` and consumes the `FLASH` peripheral singleton.

The current driver and any future external flash driver are separate types:

```text
                       embedded-storage traits
                                |
                 +--------------+--------------+
                 |                             |
       esp_hal::flash::Flash          future external driver
       internal SPI0/SPI1             SPI2/SPI3 or generic bus
       ROM, encryption, MMU           DMA and wider I/O if supported
       FLASH-DESIGN.md                FLASH-DEFERRED.md
```

[`FLASH-DEFERRED.md`](FLASH-DEFERRED.md) holds ideas that are not part of this
driver, including external SPI NOR, chip overrides, custom commands, and public
flash mapping.

## Status and stable target

Everything lands behind `unstable_driver!`. Stabilization is a separate change
after the API and hardware behavior are proven.

The intended stable surface is:

| Item | Stable target | Remains unstable |
|------|---------------|------------------|
| `Config`, `ConfigError` | yes | |
| `Flash<'d, Dm: DriverMode>` | yes | |
| `Flash::new()` returning `Flash<'d, Blocking>` | yes | |
| `Flash::apply_config()` | yes | |
| `read`, `write`, `erase`, `capacity` | yes | |
| `Error` | yes | |
| `peripherals.FLASH` | yes | |
| `erase_chip()` | | yes |
| `chip_info()`, `ChipInfo` | | yes |
| capacity override | | yes, unless stable users prove a need |
| encrypted access | | yes |
| embedded-storage trait implementations | | decision required at stabilization |
| async conversion and traits | | yes |
| `MultiCoreStrategy`, `OtherCoreRunning` | | yes |

The `Dm` parameter is part of the initial API. esp-hal mode parameters have no
default, so adding it after stabilization would break every explicit `Flash`
type. The async implementation can remain unstable without changing that type.

The stable-only use case is direct calls to `read`, `write`, and `erase`. Most
real consumers use embedded-storage traits or encrypted access. The trait
stabilization decision is therefore a required gate, not optional follow-up.

## Scope and non-goals

This driver supports only the boot flash selected by the ROM.

It does not support:

- external SPI NOR chips on SPI2/SPI3;
- custom flash commands or custom chip geometry;
- a public memory-mapping API;
- direct SPI1 register execution;
- the legacy `ReadStorage` and `Storage` read-modify-write traits.

External flash needs different configuration, errors, DMA support, and bus
ownership. It will use a separate type and share only the embedded-storage
traits with this driver. See [A12](#a12-internal-only-scope-the-external-backend-splits-out).

A boot flash that the ROM used successfully can use the ROM's standard command
set. The driver therefore has no general chip configuration. See
[A13](#a13-no-chip-configuration-at-launch).

## Compatibility requirements

The new driver must preserve the behavior on which existing users depend,
except where this document calls out a deliberate change:

- Standard read, write, and erase operations call dedicated
  `esp_rom_spiflash_*` ROM functions.
- ESP32 uses the static ROM patch library supplied by `esp-rom-sys`.
- Write and erase are split into page, sector, or block calls.
- Each ROM call has its own critical section. Interrupts run between calls.
- The cache is disabled only inside a ROM call.
- Encrypted writes keep the existing whole-sector read-modify-write behavior.
- Encrypted writes fail with `NotSupported` when flash encryption is off.
- Encrypted reads keep the private MMU path, including P4 cache invalidation.
- The bootloader's host tests remain possible after they stop depending on the
  concrete hardware type.

The new driver also fixes two unsafe buffer assumptions in `esp-storage`.
Flash-resident write data and PSRAM-resident buffers cannot be used directly
while the cache is off. The new driver stages those buffers through internal
RAM.

## Construction and detection

### `Config`

`Config` is deliberately small:

```rust
pub struct Config {
    #[cfg(multi_core)]
    multi_core_strategy: MultiCoreStrategy,
    capacity: Option<u32>,
}
```

Both fields start unstable. The stable `Config` surface is therefore empty.
`#[non_exhaustive]` leaves room for proven needs.

`multi_core_strategy` defaults to `AutoPark`. The capacity field defaults to
automatic detection. An explicit capacity is an escape hatch for a flash chip
whose JEDEC ID is missing from the detection table.

BuilderLite provides unstable setters for both fields, including
`with_multi_core_strategy(...)` and `with_capacity(...)`.

### Detection

ESP32 reads `device_id` and `chip_size` from the ROM's
`g_rom_flashchip` data. Hardware RDID is not reliable on that chip.

All other chips issue the JEDEC read-identification command through the private
`esp_rom_spiflash_read_user_cmd` binding. The returned ID is decoded with the
esptool-derived capacity table. This removes the remaining direct SPI1
register access from `esp-storage`.

An unknown ID returns `ConfigError::DetectionFailed`. Capacity zero is never a
valid driver state. An explicit capacity takes precedence over automatic
detection.

`chip_info()` reports the chip ID, capacity, and fixed geometry:

```rust
#[non_exhaustive]
pub struct ChipInfo {
    pub chip_id: u32,
    pub capacity: u32,
    pub sector_size: u32,
    pub block_size: u32,
    pub page_size: u32,
}
```

- page size: 256 bytes;
- sector size: 4096 bytes;
- block size: 64 KiB.

The reported capacity reflects the explicit override when one is set.

### Construction errors

```rust
#[non_exhaustive]
pub enum ConfigError {
    DetectionFailed,
    DriverPlacedInPsram,
}
```

`DetectionFailed` replaces the old capacity-zero failure mode.

`DriverPlacedInPsram` records the requirement that working storage used while
the cache is off must live in internal RAM. The exact way to enforce this
after Rust moves the driver remains open. See
[Open design points](#open-design-points).

## Public API

The driver owns the `FLASH` singleton, capacity, unlock state, mode marker, and
working storage:

```rust
pub struct Flash<'d, Dm: DriverMode> { /* private fields */ }

impl<'d> Flash<'d, Blocking> {
    pub fn new(flash: FLASH<'d>, config: Config)
        -> Result<Self, ConfigError>;

    #[instability::unstable]
    pub fn into_async(self) -> Flash<'d, Async>;
}

#[instability::unstable]
impl<'d> Flash<'d, Async> {
    pub fn into_blocking(self) -> Flash<'d, Blocking>;
}

impl<'d, Dm: DriverMode> Flash<'d, Dm> {
    pub fn apply_config(&mut self, config: &Config)
        -> Result<(), ConfigError>;

    pub fn read(&mut self, offset: u32, data: &mut [u8])
        -> Result<(), Error>;
    pub fn write(&mut self, offset: u32, data: &[u8])
        -> Result<(), Error>;
    pub fn erase(&mut self, from: u32, to: u32)
        -> Result<(), Error>;
    pub fn capacity(&self) -> usize;

    #[instability::unstable]
    pub fn erase_chip(&mut self) -> Result<(), Error>;
    #[instability::unstable]
    pub fn chip_info(&self) -> ChipInfo;
    #[instability::unstable]
    pub fn read_encrypted(&mut self, offset: u32, data: &mut [u8])
        -> Result<(), Error>;
    #[instability::unstable]
    pub fn write_encrypted(&mut self, offset: u32, data: &[u8])
        -> Result<(), Error>;
}
```

Blocking methods remain available in either mode. `into_async()` changes only
the type state. It does not bind an interrupt or enable DMA.

There is no `PeripheralGuard`. `FLASH` is a virtual peripheral with no clock
gate, and the driver has no drop work.

### Operation errors

```rust
#[non_exhaustive]
pub enum Error {
    IoError,
    IoTimeout,
    Locked,
    NotAligned,
    OutOfBounds,
    NotSupported,
    #[cfg(all(multi_core, feature = "unstable"))]
    OtherCoreRunning,
    Unknown,
}
```

`NotSupported` is for an environmental state, such as an encrypted write while
flash encryption is off. Standard operations have a dedicated ROM function on
every supported chip.

`Unknown` has no payload. Before returning it, the driver logs the raw ROM
return code and the ROM call that produced it.

## How an operation runs

Read, write, and erase use the same chunk loop:

```text
validate bounds and choose direct or bounce buffer
                         |
acquire flash lock and disable local interrupts
                         |
park the other core for write/erase if AutoPark applies
                         |
ROM call: cache off -> flash operation -> cache on
                         |
unpark the other core and release the lock
                         |
yield in async mode, then process the next chunk
```

The lock is acquired before the other core is parked. This prevents a frozen
core from holding the flash lock.

One chunk is one bounded ROM call. A write chunk is at most one page. An erase
chunk is one sector or block. Staged reads use the bounce-buffer size. The
maximum direct-read chunk remains open.

## Plain read, write, and erase

Bounds and alignment checks happen before a ROM call. `read` accepts byte
offsets. `write` keeps the ROM's word-alignment requirement. `erase` uses
sector alignment and may choose block erase for aligned ranges.

User buffers remain plain slices. They do not fail because of placement or
alignment:

```text
write:
  internal RAM and aligned  -> ROM
  anything else             -> 256-byte internal bounce buffer -> ROM

read:
  internal RAM and aligned  <- ROM
  anything else             <- 256-byte internal bounce buffer <- ROM
```

"Anything else" includes flash `.rodata`, PSRAM, and unaligned buffers. The
copy is a performance cost, not a different API contract.

The caller's stack must still be in internal RAM. ROM code uses that stack
while the cache is off, and the driver cannot validate it.

### Whole-chip erase

`erase_chip()` is unstable and deliberately difficult to use:

- It is one unchunked ROM call.
- Interrupts stay disabled for the whole call.
- The other core stays parked under `AutoPark`.
- Async mode cannot yield.
- No watchdog can be fed, so it may reset the device.
- The call erases the running program's own code.

It is coherent only from code and data that are fully resident in internal
RAM, such as a flasher stub.

The S2 ROM exports `SPIEraseChip` without the legacy
`esp_rom_spiflash_erase_chip` alias. `esp-rom-sys` will add that alias. Symbol
availability is established, but completion behavior still needs a destructive
hardware check. If the ROM call returns before completion, the wrapper will
call the S2 `esp_rom_spiflash_wait_idle` alias.

## Encrypted access

`read_encrypted()` maps flash pages through the private MMU path so reads are
decrypted by hardware. P4 must invalidate both L1 data cache and L2 after
changing the mapping. When flash encryption is off, the read returns the
plaintext bytes.

The ROM encrypted-write primitive requires a 32-byte-aligned address and
length. The public wrapper keeps the existing 4096-byte sector contract:

```text
read decrypted sector -> merge new bytes -> erase sector -> rewrite sector
```

This gives overwrite semantics. The operation may erase bytes outside the
requested range and restore them from the saved sector.

The storage for that full-sector read-modify-write is not yet defined. It
cannot be a PSRAM buffer while the cache is off, and this design avoids a
4096-byte stack allocation. See [Open design points](#open-design-points).

## Concurrency and async

### Multi-core writes

The intended stable behavior is `AutoPark`. For each write or erase chunk, the
driver:

1. acquires the flash lock;
2. stalls the other CPU in hardware;
3. runs the ROM call;
4. releases the other CPU;
5. releases the lock.

The stall can freeze the other core at any instruction. It may hold a lock or
be inside an interrupt handler. Stable documentation must state this clearly.

`Error` and unsafe `Ignore` strategies remain unstable. This changes the
default from `esp-storage`, which returns `OtherCoreRunning`. Migration notes
must call out that `AutoPark` stalls the other core instead of returning an
error. A future cooperative strategy may wait for the other core to reach a
safe point, but it is not part of this design.

Reads are not guarded across cores, matching `esp-storage`. Hardware is
expected to arbitrate cache refills and SPI1 reads safely. Stabilization needs
a dual-core hardware test with one core executing from flash while the other
repeats reads.

### Async behavior

Async operations call the same blocking ROM functions. They yield only between
chunks, when the cache is active again.

Dropping an async operation leaves every completed chunk in a valid state. A
later chunk may be untouched, or an erase range may be partly complete. The
async trait documentation must state this chunk-level cancellation behavior.

`Flash<'_, Async>` is `!Send` because esp-hal's `Async` mode carries that
contract. `Flash<'_, Blocking>` remains `Send`.

No watchdog is fed inside a ROM call. A future callback at chunk boundaries
could support watchdog feeding or progress reports, but that API is not
designed here.

## embedded-storage traits

The blocking traits are available for every driver mode:

```rust
impl<Dm: DriverMode> ReadNorFlash for Flash<'_, Dm> {
    const READ_SIZE: usize = 1;
}

impl<Dm: DriverMode> NorFlash for Flash<'_, Dm> {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = 4096;
}

impl<Dm: DriverMode> MultiwriteNorFlash for Flash<'_, Dm> {}
```

`MultiwriteNorFlash` applies only to plain access. Encrypted access has no
multiwrite implementation.

`NotAligned` and `OutOfBounds` map directly to the matching
`NorFlashErrorKind`. Other errors map to `Other`.

The async embedded-storage traits are implemented only for
`Flash<'_, Async>`.

The legacy `ReadStorage` and `Storage` traits are not implemented. Callers that
need a plain sub-sector update must perform the read-modify-write themselves.

The dependencies use version-suffixed names:
`embedded-storage-03` and `embedded-storage-async-04`. They are optional and
enabled by `unstable`; no new cargo feature is added. Stabilization decides
whether the blocking traits can stabilize on 0.3 or must wait for 1.0. See
[A9](#a9-embedded-storage-trait-stabilization-deferred).

## Private internals: `rom.rs` and `mmu.rs`

`rom.rs` owns:

- thin wrappers around the `esp_rom_spiflash_*` functions;
- `read_user_cmd` for private detection;
- bounds checks and chunk loops;
- lock, park, and unpark ordering;
- direct and bounce-buffer routing.

`mmu.rs` owns temporary mappings for encrypted reads and the required cache
invalidation, including the P4 variant.

Neither module is public. `esp-rom-sys` is the raw escape hatch. Every public
operation resolves to a dedicated ROM function or a real `NotSupported`
environmental case. There is no partial dispatch, `unreachable!()`, or panic
arm.

## Host tests and migration

esp-hal does not build for the host. The driver therefore cannot copy
`esp-storage`'s emulation backend.

The bootloader host tests must depend on an internal flash abstraction and a
test mock, not on `Flash` itself. Hardware migration happens after that seam is
in place.

The public `ll` module is not carried forward. Its known consumer,
`alloc_psram.rs`, moves to `Flash::write`.

## Open design points

These points must be resolved before implementation claims they are settled:

1. **Driver placement:** checking an embedded buffer in `new()` does not prove
   that Rust will not later move the `Flash` value into PSRAM. The storage or
   validation model must make the internal-RAM requirement hold at each ROM
   call.
2. **Encrypted write storage:** a partial encrypted write needs one full
   4096-byte sector across erase and rewrite. The design must define where that
   sector lives without using PSRAM or a large stack local.
3. **Capacity override and chip ID:** if an explicit capacity skips detection,
   `chip_info().chip_id` needs defined behavior.
4. **`apply_config()`:** the effect of `capacity: None` needs defined behavior.
   It could restore the detected capacity, rerun detection, or leave capacity
   unchanged.
5. **Stable recovery from detection failure:** if the capacity override remains
   unstable, the stable constructor has no stable recovery path for an unknown
   JEDEC ID. Stabilization must accept that limit or change the surface.
6. **Direct-read chunking:** aligned internal-RAM reads need a maximum ROM call
   size so interrupt latency and async yield points remain bounded.

## Validation gates

Before stabilization:

- Run the flash HIL test on ESP32 with optimization disabled.
- Confirm S2 `SPIEraseChip` completion behavior on hardware.
- Run the dual-core read test and record the result in `read` documentation.
- Test read, write, erase, detection, bounds, alignment, and buffer staging.
- Test PSRAM buffers and the final driver-placement rule where PSRAM exists.
- Test encrypted reads in the encryption-off mode used by the CI fleet.
- Exercise encrypted writes through an explicit test-only bypass of the
  production encryption guard, and separately verify that the production API
  returns `NotSupported` when encryption is off.
- Record that a true encrypted round trip still needs a board with encryption
  eFuses set.
- Build the bootloader and examples through the new API.
- Run bootloader host tests through the mock abstraction.
- Update the semver baseline before stabilization.

## Appendix A: Decision log

This appendix records the final result of each reviewed design question.
Superseded material that may still be useful lives in
[`FLASH-DEFERRED.md`](FLASH-DEFERRED.md).

### A1: No SPI1 register-exec fallback

The boot flash uses standard ROM commands. Direct register execution would add
RAM-resident bit manipulation for no current operation. The driver calls ROM
functions only. Unsupported future commands may return `NotSupported`.

### A2: ESP32 opt-level requirement dropped

The old restriction came from timing-sensitive Rust code. ESP32 now gets that
code from a precompiled static ROM library. The new thin wrappers match other
chips. A debug HIL build remains the confirmation gate.

### A3: Ops-shaped interface retained; backend enum removed

The useful rule is that every operation is total: it calls a dedicated
primitive or returns `NotSupported`. The runtime backend enum and custom
command resolution moved out with the external driver.

### A4: Multi-core stable default is AutoPark

`Error` cannot be the stable default while strategy selection is unstable.
`AutoPark` makes stable write and erase usable, but must document the
unconditional CPU stall. Reads remain unguarded and need the dual-core HIL
result.

### A5: `mmap` would be an inherent method

The original extension trait had one implementor and added no value. A future
mapping API should use inherent methods. [A16](#a16-public-mmap-cut-mmu-machinery-stays-private)
later removed mapping from the current surface.

### A6: Error type shape

The public type is `flash::Error`. `Unknown` follows esp-hal naming and has no
payload. Unexpected ROM codes are logged. A future external driver owns its
own error type.

### A7: `erase_chip` is allowed, unstable, and documented loudly

Safe range erase can already destroy the running image, so `unsafe` would not
create a useful boundary. Whole-chip erase is available but unstable, with
explicit interrupt, watchdog, multi-core, and execute-in-place warnings.

### A8: `new_spi` took a configured SPI driver; superseded

The external backend left this driver. Its ownership and bus-sharing questions
are reopened in `FLASH-DEFERRED.md`.

### A9: embedded-storage trait stabilization deferred

Trait implementations start unstable. The stabilization change must choose
between the versioned 0.3 dependency and waiting for 1.0. Either choice remains
additive.

### A10: `ll` remains private

A public low-level module would bypass bounds checks, lock state, and the
multi-core policy. `esp-rom-sys` already provides the raw interface.

### A11: Driver-built host context; superseded here

The `common_command` path has no consumer after chip configuration was removed.
The host-context evidence remains with the deferred custom-command design.

### A12: Internal-only scope; the external backend splits out

Internal and external flash share operations but not implementation
constraints. Encryption and MMU mapping exist only for internal flash. DMA,
bus sharing, and wider I/O belong to external flash. Separate types avoid a
runtime-erased configuration and error surface.

### A13: No chip configuration at launch

The ROM has already booted from the internal flash with its default commands.
General command and geometry overrides serve external chips, not the internal
driver. Only an unstable capacity escape hatch remains.

### A14: DriverMode parameter retained

The mode parameter follows esp-hal convention and cannot be added after
stabilization without breaking type annotations. Async remains useful for
yielding between flash chunks, even though it is not interrupt-driven.

### A15: S2 `erase_chip` binds the raw ROM symbol

S2 contains `SPIEraseChip` but lacks the legacy alias. `esp-rom-sys` adds
`esp_rom_spiflash_erase_chip = SPIEraseChip`. A destructive hardware check must
still confirm whether the function waits for completion.

### A16: Public `mmap` cut; MMU machinery stays private

The bootloader owns the running image mappings. Creating arbitrary mappings
would require MMU page allocation and coexistence rules that esp-hal does not
have. Private MMU code remains for encrypted reads. Candidate public APIs are
kept in `FLASH-DEFERRED.md`.

### A17: Plain-slice buffers; no aligned-buffer types

embedded-storage traits use plain slices, and alignment is a performance
property rather than a hard user requirement. The driver stages unsuitable
buffers through internal RAM.

## Appendix B: ROM capability evidence

### B1: Dedicated ROM functions and the S2 `erase_chip` gap

Read, write, sector erase, block erase, unlock, and encrypted write have
dedicated functions on all supported chips. ESP32's incomplete ROM is patched
by `esp-rom-sys/libs/esp32/libesp_rom.a`, built from the IDF v5.3.1 ROM patch.

S2 exports `SPIEraseChip = 0x400170ec`
(`esp32s2.rom.ld:612`) but omits the legacy
`esp_rom_spiflash_erase_chip` alias. IDF release/v5.2 at `72d06017df` shows:

1. `esp32s2.rom.spiflash_legacy.ld` aliases the other legacy operations.
2. IDF does not call the legacy chip-erase name.
3. The compiled ROM patch defines `esp_rom_spiflash_erase_chip` only for
   ESP32.

The missing piece is therefore an alias, not a ROM routine. This evidence
proves symbol availability. It does not replace the S2 completion test.

### B2: `read_user_cmd` availability and shape

`esp_rom_spiflash_read_user_cmd(status: *mut u32, cmd: u8)` exists on all
supported chips. Evidence includes:

- ESP32 native ROM address `0x400621b0`
  (`esp32.rom.ld:1371`);
- S2 alias of `SPI_user_command_read` at `0x40015fc8`
  (`esp32s2.rom.spiflash_legacy.ld:18`);
- P4 address `0x4fc0016c`;
- entries in each C-series and S3 ROM linker file.

The function sends an 8-bit command and reads a response. It has no address,
dummy, or write-data phase. That is enough for private JEDEC ID detection, not
a general command API.

### B3: `common_command` and the host context

This evidence moved to `FLASH-DEFERRED.md` because the current driver has no
custom-command consumer.

## Appendix C: `esp-storage` internals evidence

### C1: ROM call sites

The ROM FFI calls are in `esp-storage/src/hardware.rs:8-41`, through
`esp_rom_sys::rom::spiflash`. `ll.rs` is a thin public wrapper. ESP32 links the
static patch library; other chips resolve linker symbols. The only direct SPI1
register access is RDID in `hardware.rs:70-73`.

### C2: Chunking and critical sections

`maybe_with_critical_section` wraps each ROM call
(`hardware.rs:8-30`). Erase splits by sector or block
(`nor_flash.rs:161-187`). Write splits by sector or page
(`nor_flash.rs:124-156`, `storage.rs:48-73`). The lock is released between
chunks. Cache disable and restore happen inside the ROM function.

### C3: Locking and multicore

The critical section uses `esp_sync::RawMutex` (`lib.rs:48-58`). On multi-core
chips this is a cross-core spinlock plus local interrupt disable. It does not
stop the other core from executing from flash.

`MultiCoreStrategy` is in `common.rs:229-258`. Its hooks guard write and erase,
not read. `AutoPark` uses `CpuControl::park_core`, which applies the RTC
hardware stall. `esp-storage` parks before taking the flash lock
(`common.rs:282-289`); the new driver reverses that order.

### C4: Encrypted access and MMU

`esp_rom_spiflash_write_encrypted` requires 32-byte address and length
alignment (`ll.rs:78-80`, `esp-rom-sys/src/rom/spiflash.rs:36-38`).

`esp-storage/src/encrypted.rs:44-79` wraps it in a 4096-byte sector
read-modify-write. It returns `NotSupported` when encryption is off. Temporary
decrypted mappings are in `mmu.rs:66-134`. P4 invalidates L1 data cache and L2
in `mmu.rs:168-179`.

The existing HIL test at `hil-test/src/bin/storage.rs:43-88` runs with
encryption disabled. A test-only cfg bypasses the production guard so the ROM
encrypted-write path can still run. This does not prove a true encrypted round
trip, and production writes remain `NotSupported` when encryption is off.

### C5: Capacity detection

ESP32 reads `g_rom_flashchip.chip_size`
(`hardware.rs:54-65`, linker symbol at
`esp-rom-sys/ld/esp32/rom/esp32.rom.ld:93`).

Other chips use RDID and an esptool-derived table
(`hardware.rs:67-121`). Unknown IDs return zero
(`hardware.rs:90,118`), after which bounds checks fail every operation
(`common.rs:116-122`).

### C6: Buffers and `READ_SIZE`

Aligned buffers go directly to ROM without a residency check
(`common.rs:173-192`, `nor_flash.rs:129-137`). Unaligned and read-modify-write
paths use 4-byte and 4096-byte stack buffers (`buffer.rs:12-14`,
`storage.rs:55`).

The `bytewise-read` feature is disabled by default
(`Cargo.toml:59-65`), so default `esp-storage` has `READ_SIZE = 4`
(`nor_flash.rs:10-16`). It implements `MultiwriteNorFlash`
(`nor_flash.rs:242`).

### C7: External-backend prior art

This evidence moved to `FLASH-DEFERRED.md` with the external driver.
