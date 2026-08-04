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

Everything lands behind `unstable_driver!`, which compiles the module out
entirely unless the `unstable` feature is on. Stabilization is therefore not a
matter of removing attributes: PR F moves `flash` out of the `unstable_driver!`
block into a plain `#[cfg(flash_driver_supported)] pub mod flash;`.

The intended stable surface is:

| Item | Stable target | Remains unstable |
|------|---------------|------------------|
| `Config`, `ConfigError` | yes | |
| `Flash<'d, Dm: DriverMode>` | yes | |
| `Flash::new()` returning `Flash<'d, Blocking>` | yes | |
| `apply_config()` | yes | |
| `read`, `write`, `erase`, `capacity` | yes | |
| `Error` | yes | |
| `peripherals.FLASH` | yes | |
| `chip_info()`, `ChipInfo` | | yes |
| encrypted access | | yes |
| embedded-storage trait implementations | | yes, until embedded-storage 1.0 |
| `MultiCoreStrategy`, `OtherCoreRunning` | | yes |

The `Dm` parameter is part of the initial API. esp-hal mode parameters have no
default, so adding it after stabilization would break every explicit `Flash`
type. Only `Blocking` is constructible in this design. A possible async API is
deferred without removing the type-state parameter.

Be clear about how much this stabilizes. The stable surface is direct calls to
`read`, `write`, `erase` and `capacity`. Most real consumers reach for the
embedded-storage traits or encrypted access instead, and both stay unstable, so
`esp-bootloader-esp-idf` remains an `unstable` consumer after PR F regardless.
That is a consequence of workspace policy rather than of this driver; see
[A9](#a9-embedded-storage-trait-implementations-stay-unstable).

## Scope and non-goals

This driver supports only the boot flash selected by the ROM.

It does not support:

- external SPI NOR chips on SPI2/SPI3;
- custom flash commands or custom chip geometry;
- a public memory-mapping API;
- direct SPI1 register execution;
- the legacy `ReadStorage` and `Storage` read-modify-write traits;
- a dedicated whole-chip erase.

Whole-chip erase is excluded on execution grounds rather than scope: the ROM
operation is one unchunked call that keeps interrupts disabled, may keep the other
core parked, cannot feed a watchdog, and erases the code that invoked it. Its
useful callers are RAM-resident flasher stubs, which need a stronger execution
contract than this driver expresses. See
[A7](#a7-dedicated-whole-chip-erase-is-deferred), with the candidate API and ROM
evidence in
[`FLASH-DEFERRED.md`](FLASH-DEFERRED.md#7-dedicated-whole-chip-erase).

External flash needs different configuration, errors, DMA support, and bus
ownership. It will use a separate type and share only the embedded-storage
traits with this driver. See [A12](#a12-internal-only-scope-the-external-backend-splits-out).

A boot flash that the ROM used successfully can use the ROM's standard command
set. The driver therefore has no general chip configuration. See
[A13](#a13-no-chip-configuration-at-launch).

On ESP32 only, the driver requires an ESP-IDF-compatible second-stage bootloader,
because ESP32's ROM is the only one that does not identify the flash chip itself.
Every other supported chip works from the ROM alone. This is a documented
requirement rather than a runtime check, and it must appear in the module's
`Limitations` block.

## Compatibility requirements

The new driver must preserve the behavior on which existing users depend,
except where this document calls out a deliberate change:

- Standard read, write, and erase operations call dedicated
  `esp_rom_spiflash_*` ROM functions.
- ESP32 uses the static ROM patch library supplied by `esp-rom-sys`.
- Read, write, and erase are split into bounded chunks. `esp-storage` chunks
  reads and writes by sector and erases by sector or block; the ROM itself
  splits a write into pages internally.
- Each ROM call has its own operation guard. Interrupts run between calls.
- The operation guard suspends and restores the cache around the low-level ROM
  function, and write and erase invalidate affected mappings before returning.
  This is a **deliberate change**, not preserved behavior: `esp-storage` does
  neither for plain operations. See
  [Cache handling](#cache-handling-is-the-drivers-job).
- Encrypted writes deliberately change to the ESP-IDF contract: the caller erases
  first, and `esp-storage`'s implicit sector read-modify-write is gone. See
  [Encrypted access](#encrypted-access).
- Encrypted writes fail with `NotSupported` when flash encryption is off.
- Encrypted reads keep the private MMU path, including P4 cache invalidation.
- The bootloader's host tests remain possible after they stop depending on the
  concrete hardware type.

The new driver also fixes two unsafe buffer assumptions in `esp-storage`.
Flash-resident write data and PSRAM-resident buffers cannot be used directly
while the cache is off. The new driver stages those buffers through internal
RAM using one static bounce buffer.

## Construction and detection

### `Config`

`Config` is deliberately small:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, BuilderLite)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub struct Config {
    #[cfg(multi_core)]
    #[builder_lite(unstable)]
    multi_core_strategy: MultiCoreStrategy,
}
```

The derive set follows the developer guidelines for driver configuration and
matches `spi::master::Config`. `Default` cannot be derived until
`MultiCoreStrategy::default()` is `AutoPark`; either derive `Default` on the
enum with `#[default]` on `AutoPark`, or write `Config::default()` by hand.

The field starts unstable. The stable `Config` surface is therefore empty.
On single-core chips the whole surface is empty. `#[non_exhaustive]` leaves
room for proven needs.

`multi_core_strategy` defaults to `AutoPark`. BuilderLite provides the unstable
`with_multi_core_strategy(...)` setter. There is no capacity or chip-parameter
escape hatch. One can be designed later from a demonstrated hardware need.

The driver implements
`apply_config(&mut self, config: &Config) -> Result<(), ConfigError>` as the
developer guidelines require. It is not a formality here: the only field is a
software policy with no register side effects, so switching between `AutoPark`
and the unstable strategies at runtime is both meaningful and infallible.
Detection runs once in `new()` and is not repeated. On single-core chips the
method has nothing to apply and is trivially `Ok(())`.

### Detection

The driver identifies the chip from the 24-bit JEDEC ID held in the ROM's flash
chip structure. `new()` performs, in order:

1. refresh the cache by calling the ROM's `esp_rom_spi_flash_update_id`, on the
   nine targets that export it;
2. read `device_id` through a uniform `esp-rom-sys` accessor;
3. reject the no-response sentinels `0x000000`, `0xFFFFFF` and `0xFFFF3F`, and
   the ROM's static default `0x001540EF`;
4. decode physical capacity from the manufacturer and density bytes;
5. return `ConfigError::UnknownFlashChip` if step 3 or step 4 fails.

Step 1 issues a flash command, so it runs inside the operation guard like any
other flash access. Step 4 uses the esptool-derived capacity table.

#### Where the cached ID comes from

| | Populates `device_id` | Driver can refresh it |
|-|----------------------|-----------------------|
| Every target except ESP32 | first-stage ROM bootloader, then the second-stage bootloader again | yes, `esp_rom_spi_flash_update_id` |
| ESP32 | second-stage bootloader only | no |

Before either runs, the structure holds a statically initialized Winbond W25Q16
descriptor, reporting `device_id` `0x001540EF` and a 2 MiB capacity. That is the
value step 3 rejects: a plausible-looking wrong answer rather than an obvious one,
which is why a sentinel check alone is not enough. Addresses, disassembly and the
per-revision matrix are in
[B2](#b2-the-cached-jedec-id-and-its-provenance).

Refreshing through the ROM function rather than trusting the boot chain is what
makes detection independent of the bootloader on those nine targets, and it stays
within [A1](#a1-no-spi1-register-exec-fallback)'s ROM-functions-only rule.

**ESP32 requires an ESP-IDF-compatible second-stage bootloader.** Its ROM has no
identification function and `libesp_rom.a` has none to patch in, so this is a
documented platform requirement rather than something the driver works around.
See [A13](#a13-no-chip-configuration-at-launch).

#### Byte order

`device_id` is stored most-significant-byte first: manufacturer in bits 23:16,
memory type in bits 15:8, density in bits 7:0. Every runtime writer converges on
that layout.

The esptool capacity table and `esp-storage`'s decoder both consume the *raw*
`RDID` order instead, so porting the table unchanged reads the wrong byte:

| Field | Raw `RDID` order (esptool, `esp-storage`) | Cached `device_id` order |
|-------|------------------------------------------|--------------------------|
| Manufacturer | bits 7:0 | bits 23:16 |
| Density, standard vendors | bits 23:16 | bits 7:0 |
| Density, Adesto (`0x1F`) | bits 12:8 | bits 12:8 |

Only the Adesto middle-byte case is order-independent. This asymmetry is the
easiest way to get detection silently wrong, so the HIL test asserts decoded
capacity against the known board rather than asserting that decoding succeeded.

The ROM's static default is stored in raw order, which is why its density byte is
the invalid `0xEF` under the correct decode. That is a useful accident, not a
safety property, hence the explicit check in step 3.

#### Capacity policy

The driver reports detected physical capacity and offers no override. An
unsupported density encoding is a construction error, not a fallback to a guess.
Capacity zero is therefore never reachable, which is the state `esp-storage` can
land in today and then fail every bounds check from.

Detected capacity comes from `device_id`, never from the cached `chip_size`: the
second-stage bootloader overwrites `chip_size` with the size configured in the
binary image header, so it does not describe the physical part. ESP-IDF compares
the two and clamps to the header value; this driver deliberately does not copy
that policy.

#### Why the driver does not read the ID itself

ESP-IDF obtains the full ID through `memspi_host_read_id_hs()`, which sends
`CMD_RDID` through an initialized MSPI host's `common_command` path with a
three-byte MISO phase, reads twice, and rejects a mismatch. That host context
does not exist in an esp-hal application, and ESP-IDF itself falls back to the
cached `device_id` when the boot flash is already in octal mode.

`esp_rom_spiflash_read_user_cmd()` is not an alternative: it returns a single
response byte and cannot carry the density. The ROM refresh function in step 1
is the better instrument, because it already handles the high-speed dummy-length
case that a hand-rolled read would have to replicate. Evidence for both is in
[B2](#b2-the-cached-jedec-id-and-its-provenance).

### Construction errors

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum ConfigError {
    /// The cached flash identification is missing or its density encoding is
    /// not recognized.
    UnknownFlashChip,
}
```

The variant is deliberately coarse: it says the platform assumption did not hold,
not which byte was wrong. The raw cached value is logged before the error is
returned. A caller who wants a panic can `unwrap()`; a bootloader or recovery
tool can fall back to its own detection. Why an error rather than the panic the
original design specified is recorded in
[A13](#a13-no-chip-configuration-at-launch).

## Public API

The driver owns the `FLASH` singleton, capacity, unlock state, and mode marker.
Working storage is static internal RAM rather than part of the movable driver
value.

```rust
#[derive(Debug)]
pub struct Flash<'d, Dm: DriverMode> { /* private fields */ }

impl<'d> Flash<'d, Blocking> {
    pub fn new(flash: FLASH<'d>, config: Config)
        -> Result<Self, ConfigError>;
    pub fn apply_config(&mut self, config: &Config)
        -> Result<(), ConfigError>;

    #[ram]
    pub fn read(&mut self, offset: u32, data: &mut [u8])
        -> Result<(), Error>;
    #[ram]
    pub fn write(&mut self, offset: u32, data: &[u8])
        -> Result<(), Error>;
    #[ram]
    pub fn erase(&mut self, from: u32, to: u32)
        -> Result<(), Error>;
    pub fn capacity(&self) -> usize;

    #[instability::unstable]
    pub fn chip_info(&self) -> ChipInfo;
    #[ram]
    #[instability::unstable]
    pub fn read_encrypted(&mut self, offset: u32, data: &mut [u8])
        -> Result<(), Error>;
    #[ram]
    #[instability::unstable]
    pub fn write_encrypted(&mut self, offset: u32, data: &[u8])
        -> Result<(), Error>;
}
```

`#[ram]` marks the methods that drive flash operations. `new`, `apply_config`,
`capacity` and `chip_info` do not, and stay out of internal RAM. The rule and the
`new()` caveat are in [RAM residency](#ram-residency).

`chip_info()` reports the detected identification and the fixed geometry:

```rust
#[instability::unstable]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub struct ChipInfo {
    pub chip_id: u32,
    pub capacity: u32,
    pub sector_size: u32,
    pub block_size: u32,
    pub page_size: u32,
}
```

Geometry is fixed at 256-byte pages, 4096-byte sectors and 64 KiB blocks.
`chip_id` is manufacturer-first, matching the cached value and ESP-IDF's
`chip_id`, so `0xC86016` is GigaDevice; the documentation must say so, because
[Byte order](#byte-order) puts two conventions in play.

There is no `into_async()` method or async implementation in the current design.
Keeping the `Dm` parameter now allows one to be added later without breaking
explicit `Flash` type annotations; see
[A14](#a14-drivermode-parameter-retained).

There is no `PeripheralGuard`. `FLASH` is a virtual peripheral with no clock
gate, and the driver has no long-lived drop work.

ESP-IDF performs cleanup per operation through balanced `start` and `end`
callbacks. Those callbacks acquire and release arbitration, disable and
restore interrupts, coordinate the other core, and suspend and resume cache as
required. Cache invalidation also happens before a write or erase operation
returns. The low-level `esp_rom_spiflash_*` functions do not perform this
system-level cleanup themselves.

The new driver follows that ownership model with a short-lived internal
operation guard. The guard restores cache, interrupts, the other core, and the
flash lock on every return path. This internal guard may use `Drop`; the
long-lived `Flash` type does not.

`esp_rom_spiflash_unlock()` deliberately changes persistent flash status bits.
Dropping the driver must not try to reconstruct or restore an unknown
vendor-specific protection state. SPI1 command registers are scratch state for
the next operation, not peripheral ownership state. If a timed-out operation
has already been accepted by the flash chip, dropping a Rust value cannot
cancel it; any recovery must be explicit in that operation's error path.

### Operation errors

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum Error {
    IoError,
    IoTimeout,
    Locked,
    NotAligned,
    OutOfBounds,
    NotSupported,
    #[cfg(all(multi_core, feature = "unstable"))]
    #[cfg_attr(docsrs, doc(cfg(feature = "unstable")))]
    OtherCoreRunning,
    Unknown,
}
```

`Error` and `ConfigError` also implement `Display` and `core::error::Error`, per
the developer guidelines for error types. Feature-gating a variant of an
otherwise stable enum follows existing practice: `spi::Error` gates its DMA
variant and `uart::ConfigError` gates `BaudrateNotAchievable` the same way.

`NotSupported` is for an environmental state, such as an encrypted write while
flash encryption is off. Standard operations have a dedicated ROM function on
every supported chip.

`Unknown` has no payload. Before returning it, the driver logs the raw ROM
return code and the ROM call that produced it. See
[A6](#a6-error-type-shape).

## How an operation runs

Read, write, and erase use the same chunk loop:

```text
validate bounds and choose direct or bounce buffer
                         |
acquire flash lock and apply the other-core policy
                         |
disable local interrupts and suspend cache
                         |
call the low-level ROM flash function
                         |
invalidate affected mappings after write/erase
                         |
resume cache and interrupts, restore the other core, release the lock
```

The lock is acquired before the other core is parked. This prevents a frozen
core from holding the flash lock. One chunk is one bounded ROM call.

### Cache handling is the driver's job

The low-level ROM functions do not touch the cache. Verified from the ROM
disassembly: on ESP32 `SPIWrite`, `SPIRead`, `SPIEraseSector` and `SPIEraseBlock`
call only `SPI_page_program`, `SPI_write_enable` and `Wait_SPI_Idle`, and on C6
`_esp_rom_spiflash_write`, `_esp_rom_spiflash_read` and
`_esp_rom_spiflash_erase_sector` call only their SPI primitives and bit-length
setters. The ESP32 ROM patch source is the same: its only cache-named functions,
`spi_cache_mode_switch` and `esp_rom_opiflash_cache_mode_config`, configure SPI0's
read mode and dummy cycles rather than suspending or invalidating anything. So if
cache handling is wanted, the driver has to do it.

Whether it is *needed* is a separate question, and the honest answer is that this
driver is choosing to be stricter than the one it replaces:

| | Cache suspend around the ROM call | Invalidate after write/erase |
|-|-----------------------------------|------------------------------|
| ROM functions | no | no |
| `esp-storage`, plain operations | no, critical section only | no |
| `esp-storage`, encrypted write | no | yes (`hardware.rs:37-40`) |
| ESP-IDF | yes, per operation | yes ([B3](#b3-esp-idf-operation-cleanup)) |
| This driver | yes | yes |

`esp-storage` therefore works today without either, which is a fair challenge to
the guard. Two things justify following ESP-IDF anyway. Without invalidation, a
write or erase to a region that is currently mapped through the cache can be
followed by a cache read returning pre-write data; `esp-storage` escapes this
because storage partitions are not normally XIP-mapped, so the gap is latent
rather than absent. And its own encrypted-write path already invalidates, so the
inconsistency is inside `esp-storage` rather than between it and ESP-IDF.

This is worth revisiting with measurements if the guard turns out to dominate the
PR A throughput numbers, since it is the per-chunk cost.

### Chunk size

Chunk size is a throughput decision, not a buffer-management detail, so the paths
do not share a limit:

| Path | Chunk limit |
|------|-------------|
| Direct read, buffer already in internal RAM and aligned | 16384 bytes |
| Direct write, buffer already in internal RAM and aligned | 8192 bytes |
| Staged through the bounce buffer | the size of the bounce buffer |
| Erase | one sector or one block |

Every chunk boundary pays a full guard: take the lock, park the other core,
disable interrupts, suspend cache, call ROM, invalidate, resume, unpark, release.
So the limit on the direct path, which needs no staging at all, should not be
dictated by the bounce buffer.

The direct limits match ESP-IDF's `MAX_READ_CHUNK` and `MAX_WRITE_CHUNK`
([B6](#b6-esp-idf-chunk-sizes)) rather than `esp-storage`'s sector-sized chunking
([C2](#c2-chunking-and-critical-sections)). Adopting ESP-IDF's ceilings means
adopting the upper bound that ESP-IDF itself picked to keep the system available,
which is a better-argued number than a sector.

It does mean the interrupts-disabled and cache-off window is two to four times
longer than `esp-storage`'s on the direct path. That is the deliberate trade:
fewer guard transitions for a longer worst-case window. PR A therefore has to
measure **interrupt latency alongside throughput**, because throughput alone
cannot show this cost, and a latency regression here would be a real behavior
change for consumers with tight timing.

Both limits are private implementation policy rather than stable guarantees.
Keeping them independent also means
[`FLASH-DEFERRED.md`](FLASH-DEFERRED.md#9-configurable-bounce-storage) can revisit
the buffer size later without changing the chunking of buffers that are never
staged.

### RAM residency

The rule is by capability, not by call depth: **every method that performs, or
could perform, a flash operation carries `#[ram]`, along with everything it calls
on that path.** Methods that only touch driver state stay out of RAM.

| Item | `#[ram]` | Why |
|------|----------|-----|
| `read`, `write`, `erase` | yes | drive ROM flash operations |
| `read_encrypted`, `write_encrypted` | yes | as above, plus MMU mapping |
| The operation guard, ROM wrappers, cache and interrupt manipulation, mapping invalidation | yes | run with the cache suspended |
| embedded-storage trait methods | yes | they delegate onto the flash path |
| `capacity` | no | returns the value detected during construction; `esp-storage`'s equivalent is a field read |
| `chip_info` | no | reports stored identification and fixed geometry |
| `apply_config` | no | software policy, no register or flash access |
| `new` | no, but see below | |

`new()` is the awkward case. It does not touch flash *directly*, but detection
refreshes the cached ID through a ROM function that issues a flash command. That
refresh therefore lives behind a `#[ram]` helper, which keeps `new()` itself out
of RAM without leaving flash-touching code in a flash-resident function. If the
implementation ends up inlining the refresh into `new()`, `new()` needs the
attribute too, and the linked-section check is what catches that.

Prefer over-applying to under-applying. A method that gains a flash operation
later, or that the compiler inlines into one, is a hang rather than a test
failure, so the boundary is not worth shaving. See
[A18](#a18-ram-residency-scoped-by-flash-capability).

This is an implementation rule, not a stable user-facing guarantee. The
linked-section check verifies the RAM-resident set, which means the set has to be
named somewhere the test can assert against.

The caller's stack must still be in internal RAM, because ROM code uses that
stack while the cache is off. PSRAM-backed stacks are unsupported by
documentation; the driver does not check them at runtime.

## Plain read, write, and erase

Bounds and alignment checks happen before a ROM call. `read` accepts byte
offsets. `write` keeps the ROM's word-alignment requirement. `erase` uses
sector alignment and may choose block erase for aligned ranges.

Two different alignments are in play and only one is the caller's problem:

| Alignment | Applies to | Handling |
|-----------|-----------|----------|
| Flash offset and length | `write`, `erase` | checked, `NotAligned` on failure |
| Source or destination pointer | all operations | staged through the bounce buffer |

`esp-storage` makes the same split: `check_alignment` guards the flash offset and
length, while `is_word_aligned` on the buffer selects the staged path. Keeping
them separate in the documentation avoids implying that `WRITE_SIZE = 4` says
anything about where the caller's buffer lives.

User buffers remain plain slices, never aligned-buffer newtypes
([A17](#a17-plain-slice-buffers-no-aligned-buffer-types)). They do not fail
because of placement or alignment:

```text
write:
  internal RAM and aligned  -> ROM
  anything else             -> internal bounce buffer -> ROM

read:
  internal RAM and aligned  <- ROM
  anything else             <- internal bounce buffer <- ROM
```

"Anything else" includes flash `.rodata`, PSRAM, and unaligned buffers. The
copy is a performance cost, not a different API contract.

The bounce buffer is one private static in internal RAM. The `FLASH` singleton
and exclusive `&mut Flash` access prevent concurrent use, and moving the `Flash`
value into PSRAM does not move the buffer.

**Its size starts at 256 bytes.** That is an initial choice rather than a stable
guarantee, and it is the only place this document fixes a number: everywhere else
the staged chunk limit is "the size of the bounce buffer". The intended way to
change it is an `esp-config` option, which keeps the size out of the public API
and lets an application trade internal RAM for fewer ROM calls. Any such option
must preserve internal-RAM residency and exclusive access while the cache is
disabled. Candidate forms are in
[`FLASH-DEFERRED.md`](FLASH-DEFERRED.md#9-configurable-bounce-storage).

`READ_SIZE` is 1, so a read may start mid-word. The ROM read primitive requires
a word-aligned offset and length, so an unaligned read stages an aligned
superset through the bounce buffer and copies out the requested window. The
usable payload of a staged chunk is therefore up to three bytes less than the
buffer. Chunk arithmetic must account for that rather than assuming a full
buffer per chunk.

## Encrypted access

`read_encrypted()` maps flash pages through the private MMU path so reads are
decrypted by hardware. P4 must invalidate both L1 data cache and L2 after
changing the mapping. When flash encryption is off, the read returns the
plaintext bytes.

`write_encrypted()` follows ESP-IDF's `esp_flash_write_encrypted()` contract:

- address and length must be multiples of 16 bytes, on every chip;
- the destination must already be erased, and the driver never erases implicitly;
- the driver never changes bytes outside the requested range;
- writing an external flash chip is not supported.

The 16-byte contract is uniform by decision
([A19](#a19-encrypted-writes-match-esp-idf)), which costs a bounded row-local
decrypt and re-encrypt on ESP32; see [Row size](#row-size).

This removes `esp-storage`'s implicit 4096-byte sector read-modify-write, which
is a different thing from the ESP32 row handling above: no erase, and nothing
outside the requested range changes. A separate sector-overwrite helper can be
added later without changing this API, and is recorded in
[`FLASH-DEFERRED.md`](FLASH-DEFERRED.md#6-encrypted-sector-overwrite).

### Row size

The ROM's encryption unit is not uniform. ESP32 encrypts a 32-byte row as two
AES blocks sharing an address-derived key, while ESP32-S2 and later accept 16,
32 or 64-byte rows directly. A 16-byte-aligned request therefore lowers straight
onto the ROM call everywhere except ESP32, where the other half of the row has to
be decrypted and re-encrypted unchanged.

This is why the `esp_rom_spiflash_write_encrypted` binding documents 32-byte
alignment while ESP-IDF's public API promises 16: both are accurate, and ESP-IDF
bridges the gap.

The driver bridges it the same way. On ESP32, a request whose start or end is
16-byte but not 32-byte aligned reads the adjacent 16-byte block back decrypted
and re-encrypts it unchanged as the other half of the row, following
`esp_flash_write_encrypted` ([B5](#b5-esp-idfs-encrypted-write-row-handling)).
The public contract is therefore 16 bytes on every chip, and callers need no
`cfg`. The cost is that this read-back only round-trips correctly when encryption
is actually enabled, which is already a precondition of the method.

### Encrypted chunking

Encrypted reads map and copy at most one page-limited chunk at a time. Encrypted
writes split larger aligned inputs along the same direct and staged limits as
plain access, respecting the row size above.

## Concurrency

### Multi-core operations

The intended stable behavior is `AutoPark`. For each ROM-operation chunk, the
driver:

1. acquires the flash lock;
2. stalls the other CPU in hardware;
3. disables local interrupts and suspends cache;
4. runs the ROM call;
5. restores cache and local interrupts;
6. releases the other CPU;
7. releases the lock.

The stall can freeze the other core at any instruction. It may hold a lock or
be inside an interrupt handler. Stable documentation must state this clearly.

`Error` and unsafe `Ignore` strategies remain unstable. This changes the
default from `esp-storage`, which returns `OtherCoreRunning` only for writes
and erases. Migration notes must call out that `AutoPark` stalls the other core
for reads as well. A future cooperative strategy may wait for the other core to
reach a safe point, but it is not part of this design. See
[A4](#a4-multi-core-stable-default-is-autopark).

Reads cannot rely on SPI0/SPI1 arbitration while calling the low-level ROM
primitive. Espressif documents that the caches must be disabled for reads as well
as writes and erases, and that no CPU may be executing from flash while they are
([B7](#b7-espressifs-documented-spi1-concurrency-constraint)); arbitration is only
promised under the opt-in auto-suspend feature. ESP-IDF wraps reads in the same
guard accordingly.

Stabilization needs a dual-core hardware test that confirms `AutoPark` prevents
XIP execution on the other core during each read chunk and restores it afterward.
This is new coverage rather than a port: the existing `multicore_flash` qa-test
exercises writes only.

No watchdog is fed inside a ROM call. A future callback at chunk boundaries
could support watchdog feeding or progress reports, but that API is not
designed here.

## embedded-storage traits

The blocking traits are available for `Flash<'_, Blocking>`:

```rust
#[instability::unstable]
impl ErrorType for Flash<'_, Blocking> {
    type Error = Error;
}

#[instability::unstable]
impl ReadNorFlash for Flash<'_, Blocking> {
    const READ_SIZE: usize = 1;

    #[ram]
    fn read(&mut self, offset: u32, data: &mut [u8])
        -> Result<(), Self::Error> { /* ... */ }

    fn capacity(&self) -> usize { /* ... */ }
}

#[instability::unstable]
impl NorFlash for Flash<'_, Blocking> {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = 4096;

    #[ram]
    fn erase(&mut self, from: u32, to: u32)
        -> Result<(), Self::Error> { /* ... */ }

    #[ram]
    fn write(&mut self, offset: u32, data: &[u8])
        -> Result<(), Self::Error> { /* ... */ }
}

#[instability::unstable]
impl MultiwriteNorFlash for Flash<'_, Blocking> {}
```

`MultiwriteNorFlash` applies only to plain access. Encrypted access has no
multiwrite implementation.

`NotAligned` and `OutOfBounds` map directly to the matching
`NorFlashErrorKind`. Other errors map to `Other`.

`READ_SIZE` is 1 unconditionally. Default `esp-storage` reports 4 and only
reports 1 behind its non-default `bytewise-read` feature. Relaxing the constant
is compatible for callers, but it changes what generic storage layers see, so
migration notes should mention it and the HIL test should cover unaligned reads
by default rather than under a feature.

The legacy `ReadStorage` and `Storage` traits are not implemented. Callers that
need a plain sub-sector update must perform the read-modify-write themselves.

The dependency uses the version-suffixed name `embedded-storage-03`. It is
optional and enabled by `unstable`; no new cargo feature is added. The
implementations stay unstable, including through PR F, and the suffixed name keeps
a later 1.0 implementation additive. See
[A9](#a9-embedded-storage-trait-implementations-stay-unstable).

## Private internals: `rom.rs` and `mmu.rs`

`rom.rs` owns:

- thin `#[ram]` wrappers around the `esp_rom_spiflash_*` functions;
- the `#[ram]` operation guard: cache suspend and resume, interrupt disable and
  restore, park and unpark, mapping invalidation;
- private access to the ROM-cached flash ID and its byte-order normalization;
- bounds checks and chunk loops;
- lock, park, and unpark ordering;
- direct and bounce-buffer routing;
- the static internal-RAM bounce buffer.

Only the first two items are RAM-resident. The rest runs with the cache enabled.

`mmu.rs` owns temporary mappings for encrypted reads and the required cache
invalidation, including the P4 variant.

Neither module is public, and no low-level module replaces `esp-storage`'s `ll`;
`esp-rom-sys` is the raw escape hatch
([A10](#a10-ll-remains-private)). Every operation resolves to a dedicated ROM
function or a real `NotSupported` environmental case
([A3](#a3-ops-shaped-interface-retained-backend-enum-removed)). There is no
partial dispatch, `unreachable!()`, or panic arm.

## Consumers and migration

### The bootloader's partition accessors

`esp-bootloader-esp-idf` is the driver's most demanding consumer, because a
partition is either effectively plaintext or effectively encrypted and the two
support different operations. It exposes two direct, checked conversions from a
partition entry:

```text
PartitionEntry::as_flash_region()           -> FlashRegion
PartitionEntry::as_encrypted_flash_region() -> EncryptedFlashRegion
```

`FlashRegion` is guaranteed effectively plaintext and implements the blocking NOR
traits directly. `EncryptedFlashRegion` is guaranteed effectively encrypted and
exposes inherent `read`, `erase` and encrypted write methods only. It does not
implement `NorFlash` or `MultiwriteNorFlash`, because a physical erase does not
read back as plaintext `0xFF` through the decryption view.

Both accessors check effective encryption from the partition flags, the partition
type and the hardware encryption state. Higher-level flows such as OTA own that
policy choice and pick the accessor; any enum needed to hide a runtime choice
stays private to the bootloader crate. There is no intermediate dynamic
`FlashRegion` followed by `NorFlashRegion` or `EncryptedNorFlashRegion`.

### Host tests

esp-hal does not build for the host, so the driver cannot copy `esp-storage`'s
emulation backend. The bootloader host tests must therefore depend on an internal
flash abstraction and a test mock rather than on `Flash` itself, and the hardware
migration happens only after that seam is in place.

### Migration is per binary, not incremental

Both drivers consume the same peripheral singleton. `esp-storage` re-exports it as
`esp_storage::Flash` (`common.rs:6`) and `FlashStorage::new` takes it by value
(`common.rs:92`); `esp_hal::flash::Flash::new` takes the same `FLASH<'d>`. A binary
can therefore hold one or the other, never both.

That is intended rather than an obstacle: it is the mechanism that stops two
drivers racing on one flash chip, and it falls out of the singleton model for free.
The consequence for users is that a crate cannot be migrated a call site at a time.
The switch is per binary, including any dependency that reaches for
`esp-storage` itself, so the migration guide has to say so plainly. It is also why
`esp-storage`'s HIL test and the new `flash.rs` test stay separate binaries rather
than one.

### Retired surface

The public `ll` module is not carried forward. Its known consumer,
`alloc_psram.rs`, moves to `Flash::write`.

## Validation gates

Before stabilization:

- Run the flash HIL test on ESP32 with optimization disabled.
- Run the dual-core read test and verify `AutoPark` restoration.
- Test read, write, erase, detection, bounds, alignment, and buffer staging.
- Verify detection rejects the no-response sentinels, the ROM's static W25Q16
  default `0x001540EF`, and unsupported density encodings, and that `new()`
  returns `ConfigError::UnknownFlashChip` rather than panicking.
- Confirm the ROM refresh path works: `esp_rom_spi_flash_update_id` links and
  produces the expected ID on all nine targets that export it, including in an
  octal boot mode where its dummy-length branch is taken. On ESP32, confirm the
  cached ID is correct under the ESP-IDF bootloader, which is the documented
  requirement there.
- Verify the ESP32 bootloader requirement is stated in the module `Limitations`
  block and on `new()`.
- Assert that decoded capacity equals the *known* capacity of each board, on
  every supported chip family and including octal-flash boot modes. Asserting
  only that decoding succeeded would pass with the byte order reversed.
- Test direct and bounced reads and writes across their respective chunk
  boundaries, and unaligned reads whose aligned superset crosses a boundary.
- Test flash-resident and PSRAM buffers where PSRAM exists.
- Verify from linked sections that every flash-operating method, the guard and the
  ROM wrappers land in a RAM section, and that `new`, `apply_config`, `capacity`
  and `chip_info` do not. If the detection refresh gets inlined into `new`, this
  check is what catches it.
- Measure read and write throughput against `esp-storage` on one RISC-V and one
  Xtensa target. A large regression means the chunk policy is wrong.
- Measure worst-case interrupt latency during a large read and a large write on
  the same targets. The direct chunk limits are two to four times
  `esp-storage`'s, so this is where that trade shows up, and throughput alone
  cannot reveal it.
- Test encrypted reads in the encryption-off mode used by the CI fleet.
- Exercise encrypted writes through an explicit test-only bypass of the
  production encryption guard, and separately verify that the production API
  returns `NotSupported` when encryption is off.
- Verify that encrypted writes reject ranges not aligned to 16 bytes, never erase
  implicitly, and operate on previously erased 16-byte-aligned ranges on every
  chip.
- On ESP32, exercise a 16-byte-aligned but not 32-byte-aligned encrypted write at
  both the start and the end of a range, and confirm the neighbouring 16-byte
  block is unchanged afterwards. That is the pre/post read-back path, and
  corrupting the neighbour is its failure mode.
- Record that a true encrypted round trip still needs a board with encryption
  eFuses set.
- Build the bootloader and examples through the new API.
- Run bootloader host tests through the mock abstraction.
- Update the semver baseline before stabilization.

The design questions that were open during review are now closed: the
encrypted-write alignment contract is a uniform 16 bytes
([A19](#a19-encrypted-writes-match-esp-idf)), the direct chunk limits follow
ESP-IDF's ceilings ([Chunk size](#chunk-size)), and the embedded-storage
implementations stay unstable
([A9](#a9-embedded-storage-trait-implementations-stay-unstable)). What remains is
measurement, not choice.

## Appendix A: Decision log

This appendix records the final result of each reviewed design question.
Superseded material that may still be useful lives in
[`FLASH-DEFERRED.md`](FLASH-DEFERRED.md).

### A1: No SPI1 register-exec fallback

The boot flash uses standard ROM commands and the cached ID. Direct register
execution would add RAM-resident bit manipulation for no current operation. The
driver calls ROM functions only. Unsupported future commands may return
`NotSupported`.

`esp-storage` reads `RDID` from SPI1 registers itself, so this rule looks like it
costs a capability. It does not: the ROM's own `esp_rom_spi_flash_update_id`
provides the same bootloader independence through a ROM function on nine of the
ten targets, and ESP32 is covered by the documented bootloader requirement in
[A13](#a13-no-chip-configuration-at-launch).

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
`AutoPark` makes stable read, write, and erase usable, but must document the
unconditional CPU stall around every low-level ROM operation.

**Reads take the same guard, and this is a requirement rather than a preference.**
Espressif documents the SPI0/SPI1 constraint as covering "any operations like
read/write/erase", says the caches "must be disabled while reading/writing/erasing",
and says that while they are disabled "all CPUs should always execute code and
access data from internal RAM" ([B7](#b7-espressifs-documented-spi1-concurrency-constraint)).
Hardware arbitration between the cache and SPI1 is only promised under
`CONFIG_SPI_FLASH_AUTO_SUSPEND`, which is opt-in, chip-dependent and off by
default. There is no default-configuration guarantee to lean on.

`esp-storage` does not guard reads at all, so by that documented constraint its
reads are a latent bug rather than a behavior worth preserving. It survives
because the window is short and the collision is data-dependent, not because the
hardware promises anything. The existing `qa-test/src/bin/multicore_flash.rs` does
not contradict this: it drives cache pressure on one core against
`write_nor` on the other and never tests a concurrent flash *read*, and it
excludes ESP32 and P4 outright with a "TODO: Make esp32 work".

That gives the migration note a real reason. Consumers doing large dual-core reads
will see new stalls, and the honest framing is that the old behavior was
under-guarded, not that the new driver is being pedantic.

The cost is worth stating plainly, because it compounds with
[Chunk size](#chunk-size): a 16384-byte read chunk now stalls the other core for
the duration of a 16 KiB flash read, and reads are the commonest operation. If the
PR A interrupt-latency numbers look bad, the read chunk limit is the dial to turn,
not the guard.

A principled way out exists and is deferred rather than dismissed: on chips and
flash parts supporting program/erase suspend, the guard can be dropped the way
ESP-IDF's auto-suspend does. See
[`FLASH-DEFERRED.md`](FLASH-DEFERRED.md#10-relax-the-guard-via-flash-auto-suspend).

### A5: `mmap` would be an inherent method

The original extension trait had one implementor and added no value. A future
mapping API should use inherent methods. [A16](#a16-public-mmap-cut-mmu-machinery-stays-private)
later removed mapping from the current surface.

### A6: Error type shape

The public type is `flash::Error`. `Unknown` follows esp-hal naming and has no
payload. Unexpected ROM codes are logged. A future external driver owns its
own error type.

### A7: Dedicated whole-chip erase is deferred

Absent on execution grounds, as [Scope and non-goals](#scope-and-non-goals)
states: it cannot be chunked, keeps the system unavailable for an unbounded
period, and destroys the running image. The candidate API and the ROM evidence,
including the ESP32-S2 symbol gap, live in
[`FLASH-DEFERRED.md`](FLASH-DEFERRED.md#7-dedicated-whole-chip-erase).

### A8: `new_spi` took a configured SPI driver; superseded

The external backend left this driver. Its ownership and bus-sharing questions
are reopened in `FLASH-DEFERRED.md`.

### A9: embedded-storage trait implementations stay unstable

The trait implementations stay unstable, including through PR F, and become
stabilization candidates when embedded-storage reaches 1.0.

This is not really a flash decision; esp-hal already has a rule and it is
consistent. `embedded-hal` and `embedded-hal-async` at 1.0 are unconditional,
plain-named dependencies whose implementations are part of the stable surface.
Every pre-1.0 ecosystem trait crate is version-suffixed, optional and gated behind
`unstable`: `embedded-io` 0.6 and 0.7, `rand_core` 0.6, 0.9 and 0.10,
`embedded-can`, `nb`. Nothing pre-1.0 has ever been stabilized. Stabilizing the
0.3 implementations would make flash the exception and set a workspace precedent
on the back of one driver.

The version-suffixed dependency name keeps that additive: esp-hal already ships
two `embedded-io` majors side by side, so a later 1.0 implementation can land
without disturbing the 0.3 one.

The consequence is that PR F stabilizes only the inherent methods, and the
bootloader stays an `unstable` consumer because of the encrypted API anyway. An
earlier revision of the plan claimed most of the practical de-unstabling value sat
in this decision; on this reading it does not, so PR F is worth doing for the
inherent surface or not at all.

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

The boot chain has already booted from the internal flash with its default
commands. General command and geometry overrides serve external chips, not the
internal driver. There is no capacity escape hatch. Any future exception must
start from a demonstrated chip and specify more than an assumed capacity when
its behavior differs from the standard ROM operations.

**Failure is an error, not a panic.** The original design panicked, on the
grounds that the first-stage ROM guarantees a valid cache. That premise holds on
nine of the ten targets but not on ESP32
([B2](#b2-the-cached-jedec-id-and-its-provenance)). An error is the right choice
even so: the developer guidelines prefer a fallible API when a `Result` is
already being returned, ESP32 leaves a real if narrow failure path, and the
pre-detection value is a plausible 2 MiB descriptor rather than an obvious
sentinel, so a driver that assumed success could serve a wrong capacity instead of
failing loudly.

**ESP32 requires an ESP-IDF-compatible second-stage bootloader.** Settled, not an
open question. ESP32's ROM has no identification function, `libesp_rom.a` never
had one to patch in, and the remaining routes are either single-byte or
register-level. Since any espflash-flashed image carries that bootloader, and
ESP32 is old enough that new bootloaders for it are unlikely, documenting the
requirement beats carrying a chip-specific detection path. It must appear in the
module's `Limitations` block and on `new()`.

If that ever stops being acceptable, exporting the ROM's `SPI_Common_Command` on
ESP32 is the escalation, because it stays within
[A1](#a1-no-spi1-register-exec-fallback).

### A14: DriverMode parameter retained

The mode parameter follows esp-hal convention and cannot be added after
stabilization without breaking type annotations. Only the blocking state is
constructible. Async conversion and operation semantics are deferred until
there is a real use case.

### A16: Public `mmap` cut; MMU machinery stays private

The bootloader owns the running image mappings. Creating arbitrary mappings
would require MMU page allocation and coexistence rules that esp-hal does not
have. Private MMU code remains for encrypted reads. Candidate public APIs are
kept in `FLASH-DEFERRED.md`.

### A17: Plain-slice buffers; no aligned-buffer types

embedded-storage traits use plain slices, and alignment is a performance
property rather than a hard user requirement. The driver stages unsuitable
buffers through one static internal-RAM bounce buffer.

### A18: RAM residency scoped by flash capability

`#[ram]` goes on every method that performs or could perform a flash operation,
and on everything it calls on that path. It does not go on methods that only read
back driver state: `capacity`, `chip_info` and `apply_config` touch neither
registers nor flash.

Two narrower rules were considered and rejected. The original design placed every
public method and its whole call path in RAM, which pulls state-only accessors
into scarce internal RAM for nothing. A later revision scoped it to just the
cache-off window, on the theory that the chunk loop runs with the cache enabled
between chunks. That is true in the abstract but too sharp to police: inlining can
move a flash operation into a caller that was reasoned to be safe, and getting it
wrong produces a hang rather than a failed assertion. Capability is the boundary a
reviewer and a linked-section check can both apply.

`new()` sits on the line, because detection refreshes the cached ID through a ROM
function that issues a flash command. Keeping that refresh in a `#[ram]` helper
keeps `new()` out of RAM.

This is an implementation rule verified from linked sections, not a stable
placement guarantee. The driver value may move because working storage is a static
internal-RAM buffer. PSRAM-backed stacks remain unsupported.

### A19: Encrypted writes match ESP-IDF

`write_encrypted` programs previously erased flash and never erases implicitly.
A separate sector-overwrite helper remains deferred.

**Decision: match ESP-IDF.** The contract is 16-byte alignment on every chip, and
ESP32 implements the pre/post decrypted read-back to satisfy its 32-byte ROM row.
The rejected alternative was a per-chip alignment constant, 32 on ESP32 and 16
elsewhere; it avoids the hidden read-back but stops the contract being uniform and
pushes a `cfg` onto every caller. The bootloader's OTA path is the dominant
consumer and that constant would have leaked into it.

The mechanism is in [Row size](#row-size), the ESP-IDF implementation in
[B5](#b5-esp-idfs-encrypted-write-row-handling).

This narrows the "no read-modify-write" rule to what it is really protecting: no
implicit erase, and no change to bytes outside the requested range. The ESP32
read-back satisfies both, since it rewrites the neighbouring block with exactly
the bytes it read.

## Appendix B: ROM capability evidence

### B1: Dedicated ROM functions

Read, write, sector erase, block erase, unlock, and encrypted write have
dedicated functions on all supported chips. ESP32's incomplete ROM is patched
by `esp-rom-sys/libs/esp32/libesp_rom.a`, built from the IDF v5.3.1 ROM patch.
Whole-chip erase evidence is kept with that deferred API in
`FLASH-DEFERRED.md`.

### B2: The cached JEDEC ID and its provenance

Evidence below comes from the ROM ELFs in the `esp-rom-elfs` `20260528` release,
disassembled with `llvm-objdump` for RISC-V and `xtensa-esp32-elf-objdump` for
Xtensa. Every supported chip and every silicon revision in that release was
checked.

**The ROM's static default.** `.data_spi_flash` in all 17 ROM ELFs contains the
byte pattern `ef401500 00002000 00000100 00100000 00010000 ffff0000`, which is
`esp_rom_spiflash_chip_t` pre-initialized to a Winbond W25Q16: `device_id`
`0x001540EF`, `chip_size` `0x00200000`, `block_size` `0x00010000`,
`sector_size` `0x00001000`, `page_size` `0x00000100`, `status_mask` `0x0000FFFF`.
On ESP32 the symbol at `g_rom_flashchip`'s address (`0x3ffae270`) is literally
named `spi_w25q16`. On the newer chips the same bytes live in
`rom_default_spiflash_legacy_data`, which `rom_spiflash_legacy_data` initially
points at.

**The first-stage ROM does detect.** The ROM exports
`esp_rom_spi_flash_update_id` on every supported target except ESP32, and
`ets_run_flash_bootloader` - the first-stage function that loads the second-stage
image out of flash - calls it. The `esp-rom-sys` linker scripts already provide
the symbol, and every address matches the ROM ELF:

| Target | Revisions checked | `esp-rom-sys` symbol | Implementation | Called from |
|--------|-------------------|----------------------|----------------|-------------|
| ESP32-S2 | rev0 | `esp32s2.rom.ld:210` = `0x40016e44` | `0x40016e44` | `ets_run_flash_bootloader`, also `ets_unpack_flash_code_legacy` |
| ESP32-S3 | rev0 | `esp32s3.rom.ld:167` = `0x40000a8c` | `0x4004a61c` | `ets_run_flash_bootloader` |
| ESP32-C2 | rev100, rev200 | `esp32c2.rom.ld:121` = `0x40000160` | `0x4005adda`, `0x4006230c` | `ets_run_flash_bootloader` |
| ESP32-C3 | rev0, rev101, rev3 | `esp32c3.rom.ld:117` = `0x4000014c` | `0x4004d260`, `0x4004f5ee`, `0x4004db12` | `ets_run_flash_bootloader` |
| ESP32-C6 | rev0 | `esp32c6.rom.ld:133` = `0x40000174` | `0x40024b36` | `ets_run_flash_bootloader` |
| ESP32-C5 | rev0, rev100 | `esp32c5.rom.ld:144` = `0x40000184` | `0x40027c1c`, `0x400296f0` | `ets_run_flash_bootloader` |
| ESP32-C61 | rev100 | `esp32c61.rom.ld:138` = `0x40000184` | `0x40027ecc` | `ets_run_flash_bootloader` |
| ESP32-H2 | rev0 | `esp32h2.rom.ld:124` = `0x4000016c` | `0x4000d626` | `ets_run_flash_bootloader` |
| ESP32-P4 | rev0, rev300 | `esp32p4.rom.ld:135` = `0x4fc0017c` | `0x4fc0f000`, `0x4fc0fc46` | `ets_run_flash_bootloader` |
| ESP32 | rev0, rev300 | **absent** | **absent** | n/a |

Except on S2, the linker-script address is the `__call_esp_rom_spi_flash_update_id`
thunk in the ROM's fixed jump table, which is why it is stable across revisions
while the implementation address moves. The driver can therefore call it without
new linker work, only a binding.

The C6 body (`0x40024b36`) does exactly what a detection routine should:

```text
lw   a3, rom_spiflash_legacy_data   # the chip struct
sw   zero, 0x58(SPI1)               # clear W0
sw   0x10000000, 0x0(SPI1)          # SPI_CMD, bit 28 = FLASH_RDID
lw   a4, 0x0(SPI1); bnez            # poll until the command clears
lw   a5, 0x58(SPI1)                 # read the 24-bit response
...                                 # byte-swap, see below
sw   a5, 0x0(a3)                    # store into device_id
```

It also branches on `dummy_len_plus[1]` to handle the high-speed/octal case,
which a hand-rolled register read would have to replicate.

**Nothing else in the ROM writes the field.** The only ROM caller of
`esp_rom_spiflash_config_param` (`SPIParamCfg` on ESP32) is
`FlashDwnLdParamCfgMsgProc`, the serial download-mode message handler, which
takes its values from the host packet. On ESP32 every other reference to
`spi_w25q16` is a read; the stores near those sites go to the stack or to the
SPI register base, and `SPIRead` reads offset 4 for its bounds check.

So on ESP32 specifically, nothing populates `device_id` before the second-stage
bootloader does. This holds on both rev0 and rev300, so it is a property of the
ESP32 ROM rather than of one silicon revision. It is consistent with ESP32 being
the chip that needs `libesp_rom.a` at all, and with `esp-storage`'s comment that
the hardware `RDID` path is unreliable there.

**ESP32's options for detecting the ID itself**, if the bootloader dependency
ever needs removing:

| Route | Available | Notes |
|-------|-----------|-------|
| ROM `esp_rom_spi_flash_update_id` | no | absent from the ROM on rev0 and rev300 |
| The `esp-rom-sys` ROM patch library | no | `libesp_rom.a` defines no read-ID function, and IDF's `esp_rom_spiflash.c` patch source contains no `RDID` at all |
| `esp_rom_spiflash_read_user_cmd` | yes, but useless here | the ROM hardcodes `SPI_MISO_DLEN = 7`, so it returns exactly one byte and cannot carry the density |
| ROM `SPI_Common_Command` | in ROM at `0x4006246c`, stable across rev0 and rev300, but **not** exported by `esp-rom-sys/ld/esp32/` | the ROM-function-shaped answer; would need a linker entry and a signature |
| Second-stage bootloader | yes | `bootloader_flash_update_id()`, which runs for any espflash-flashed image |
| Hand-rolled SPI1 `RDID` | yes | the `flash_rdid` command bit exists in the ESP32 register block, but `esp-storage` deliberately avoids this path on ESP32 and it would breach [A1](#a1-no-spi1-register-exec-fallback) |

The patch library is worth being explicit about, because it looks like the
obvious place to solve this: it patches ESP32's incomplete ROM for read, write,
erase, unlock and encrypted write, but it never had an identification function to
patch in.

**Decision:** ESP32 requires an ESP-IDF-compatible second-stage bootloader, and
that requirement is documented rather than engineered around. `new()` reports
`ConfigError::UnknownFlashChip` if the cache is still the ROM default. Exporting
`SPI_Common_Command` is the preferred escalation if this ever proves
insufficient, because it stays within the ROM-functions-only rule. See
[A13](#a13-no-chip-configuration-at-launch).

**The second-stage bootloader repeats the work.** `bootloader_flash_update_id()`
performs `device_id = bootloader_read_flash_id()`, defined in
`components/bootloader_support/bootloader_flash/src/bootloader_flash_config_<chip>.c`
in ESP-IDF v5.3.1 and called from
`components/bootloader_support/src/<chip>/bootloader_<chip>.c`: ESP32 `:38` from
`:215`, S2 `:35` from `:150`, S3 `:38` from `:187`, C3 `:32` from `:165`, C6
`:28` from `:150`, P4 `:25` from `:154`. So on ESP32 this is the only writer, and
on the others it is belt and braces. The same files define
`bootloader_flash_update_size()`, which sets `chip_size` from the image header.

`bootloader_flash_xmc_startup()` in
`components/bootloader_support/bootloader_flash/src/bootloader_flash.c:736-773`
also rewrites `device_id`, which is the vendor startup correction the detection
section relies on.

**Byte order, confirmed four ways.** The C6 ROM computes
`(byte0 << 16) | (byte1 << 8) | byte2` from the raw response, where byte0 is the
manufacturer, so the stored value is manufacturer-first. IDF's
`bootloader_read_flash_id()` (`bootloader_flash.c:680-685`) applies the identical
`((id & 0xff) << 16) | ((id >> 16) & 0xff) | (id & 0xff00)`. The ROM patch's ISSI
check tests `((chip->device_id >> 16) & 0xff) == 0x9D`
(`components/esp_rom/patches/esp_rom_spiflash.c:29`). And in-tree,
`esp-hal/src/psram/esp32.rs:227` declares `FLASH_ID_GD25LQ32C: u32 = 0xC86016`
and compares it against `g_rom_flashchip.device_id`, with GigaDevice's `0xC8` in
the top byte.

esptool's decoder
(`esptool/cmds.py:342-353` at `8363cae8`) takes `vendor_id = flash_id & 0xFF`
and `size_id = flash_id >> 16` from the *raw* response, as does
`esp-storage/src/hardware.rs:80-93`. Adesto (`0x1F`) reads the density from
bits 12:8 in both orders. esptool also treats `0xFFFF3F` as a no-response value
(`cmds.py:1128`) alongside `0x000000` and `0xFFFFFF`.

**Accessor shape.** The declaration is direct on ESP32 and ESP32-S2:
`esp32.rom.ld:93` provides `g_rom_flashchip` and
`esp32s2.rom.spiflash_legacy.ld:12` aliases it to `SPI_flashchip_data`. The
other eight targets provide `rom_spiflash_legacy_data`, which is a **pointer**
to the legacy data structure; ESP-IDF accesses
`rom_spiflash_legacy_data->chip.device_id`
(for example `bootloader_flash_config_esp32c6.c:109`). The accessor therefore
needs one extra indirection on those targets. `esp-hal/src/psram/esp32.rs:265`
already declares the full six-field `esp_rom_spiflash_chip_t` layout and should
move onto the shared accessor.

**Why not read RDID ourselves.** ESP-IDF v6.0.2's `memspi_host_read_id_hs()`
in `components/spi_flash/memspi_host_driver.c:93-110` sends `CMD_RDID` through
`common_command` with `miso_len = 3`, rejects `0` and `0xFFFFFF`, and byte-swaps
the result. `esp_flash_read_chip_id()` in
`components/spi_flash/esp_flash_api.c` reads twice and rejects a mismatch;
`esp_flash_init()` and `esp_flash_init_main()` retry that transient result.
`esp_flash_init_main()` bypasses ordinary `RDID` in octal mode and falls back to
`g_rom_flashchip.device_id`.

`esp_rom_spiflash_read_user_cmd(status: *mut u32, cmd: u8)` is provided on all
ten targets (`esp32.rom.ld:1371`, `esp32s2.rom.spiflash_legacy.ld:18`,
`esp32s3.rom.ld:163`, and the corresponding lines in the RISC-V scripts), but it
returns only one response byte, so it can read a status-like byte and not the
three-byte JEDEC ID. This is now confirmed rather than assumed: the ESP32
implementation (`SPI_user_command_read`, `0x400621b0`) writes the immediate `7`
to `SPI_MISO_DLEN`, which is 8 bits.

So the cache remains the right source for an internal-only driver: it identifies
the chip that supplied the running image, it is the value ESP-IDF itself falls
back to in octal mode, and it avoids depending on an application-initialized MSPI
host context. Better still, on nine targets the driver can refresh it through
`esp_rom_spi_flash_update_id` rather than trusting whoever booted us, which is
strictly stronger than `esp-storage`'s hand-rolled register read and stays within
the ROM-functions-only rule. ESP32 keeps the bootloader dependency, which is why
detection stays fallible. PR A adds the uniform accessor and validates decoded
capacity against known boards.

### B3: ESP-IDF operation cleanup

ESP-IDF's `components/spi_flash/include/esp_private/spi_flash_os.h` assigns
cache and interrupt preparation to guard callbacks around ROM functions. Its
`spi_flash_os_func_noos.c` suspends and resumes cache in `start` and `end`.
The application variant in `spi_flash_os_func_app.c` additionally owns bus
arbitration, scheduler handling, interrupts, and other-core coordination.

`esp_flash_api.c` balances those callbacks around each read chunk and around
write and erase operations, including error returns. Modified mappings are
flushed or invalidated before the operation completes. This is per-operation
state, not lifetime state of the main flash object.

`esp_flash_deinit_os_functions()` releases dynamically allocated OS context
for registered flash devices. The main boot flash uses static context, and the
new internal driver allocates or registers no equivalent resource. This
deinitialization path is therefore not drop work for `Flash`.

### B4: `common_command` and the host context

This evidence moved to `FLASH-DEFERRED.md` because the current driver has no
custom-command consumer.

### B5: ESP-IDF's encrypted-write row handling

`esp_flash_write_encrypted()` in
`components/spi_flash/esp_flash_api.c:1216-1330` (ESP-IDF v5.3.1) enforces
`address % 16 == 0` and `length % 16 == 0`, then lowers the request onto rows
whose size depends on the target.

Its own comment states that on ESP32 each call to the ROM primitive takes a
32-byte row consisting of two 16-byte AES blocks that share a key derived from
the flash address. For ESP32-S2 and later it selects 64, 32, or 16-byte rows
from the address and remaining length, bounded by
`SOC_FLASH_ENCRYPTED_XTS_AES_BLOCK_MAX`.

The ESP32 path handles a 16-byte-aligned request that is not 32-byte aligned by
reading the neighbouring block back decrypted before the loop:

```c
if ((address % 32) != 0) {
    esp_flash_read_encrypted(chip, address - 16, pre_buf, 16);
}
if (((address + length) % 32) != 0) {
    esp_flash_read_encrypted(chip, address + length, post_buf, 16);
}
```

Those blocks are then re-encrypted unchanged as the other half of the 32-byte
row. This is why `esp-rom-sys`'s 32-byte alignment note and ESP-IDF's 16-byte
public contract are both accurate, and it is the work the driver has to do if it
wants the uniform contract.

The same function sets `lock_once = esp_ptr_in_dram(buffer)` and re-acquires the
guard per row when the source buffer is not in internal RAM, because it must copy
from flash with the cache enabled. That is independent confirmation of this
design's bounce-buffer requirement.

### B6: ESP-IDF chunk sizes

`components/spi_flash/esp_flash_api.c:32-37` defines `MAX_WRITE_CHUNK` as 8192
(or `CONFIG_SPI_FLASH_WRITE_CHUNK_SIZE`) and `MAX_READ_CHUNK` as 16384. Reads
apply it at line 871 and writes at lines 1035-1045, with the stated purpose of
bounding how long the system stays unavailable. These are the upper bounds a
comparable driver is measured against.

### B7: Espressif's documented SPI1 concurrency constraint

`docs/en/api-reference/peripherals/spi_flash/spi_flash_concurrency.rst` in ESP-IDF
v5.3.1 states the constraint directly, and it names reads:

> The SPI0/1 bus is shared between the instruction & data cache (for firmware
> execution) and the SPI1 peripheral [...] This kind of operations include calling
> SPI Flash API or other drivers on SPI1 bus, **any operations like
> read/write/erase** or other user defined SPI operations

> On {IDF_TARGET_NAME}, these caches **must be disabled while reading**/writing/erasing.

And for the multi-core consequence:

> Under this condition, **all CPUs** should always execute code and access data
> from internal RAM.

`index.rst:246-251` describes the mechanism: the SDK uses `esp_ipc_call` to run
`spi_flash_op_block_func` on the other CPU, which disables that CPU's cache and
sets `s_flash_op_can_start`, before the calling CPU disables its own cache and
proceeds.

Concurrent cache access is permitted only where a named mechanism replaces the
software guard:

| Mechanism | Availability |
|-----------|--------------|
| `CONFIG_SPI_FLASH_AUTO_SUSPEND` | needs `SOC_SPI_MEM_SUPPORT_AUTO_SUSPEND` and a flash chip supporting program/erase suspend; disabled by default |
| `CONFIG_SPIRAM_XIP_FROM_PSRAM` | needs `SOC_SPIRAM_XIP_SUPPORTED`; disabled by default |
| `CONFIG_APP_BUILD_TYPE_RAM` | whole application in RAM, so nothing executes from flash |

Only under auto-suspend does the documentation say "the hardware will handle the
arbitration between them". That is the sole basis on which SPI0/SPI1 hardware
arbitration may be relied upon, and it is opt-in and chip-dependent. In the
default configuration there is no such guarantee, for reads or anything else.

This is the evidence behind [A4](#a4-multi-core-stable-default-is-autopark), and
it is stronger than an inference from ESP-IDF's lock structure: Espressif
documents the requirement rather than merely implementing it.
## Appendix C: `esp-storage` internals evidence

### C1: ROM call sites

The ROM FFI calls are in `esp-storage/src/hardware.rs:8-41`, through
`esp_rom_sys::rom::spiflash`. `ll.rs` is a thin public wrapper. ESP32 links the
static patch library; other chips resolve linker symbols. The only direct SPI1
register access is RDID in `hardware.rs:70-73`.

### C2: Chunking and critical sections

`maybe_with_critical_section` wraps each ROM call
(`hardware.rs:8-41`). Erase splits by sector or block
(`nor_flash.rs:157-186`).

Reads and writes split by **sector**, never by page: `write_nor` steps by
`SECTOR_SIZE` on both the aligned and the staged path
(`nor_flash.rs:131-152`), and `storage.rs:16-33` and `:57-70` do the same for
`read` and `write`. So `esp-storage`'s per-ROM-call payload is 4096 bytes, and
the ROM splits a write into 256-byte pages internally. An earlier revision of
this document described the split as "sector or page", which made a 256-byte
chunk look like parity when it is a sixteenfold reduction.

The lock is released between chunks. This critical section does not provide
ESP-IDF's explicit cache suspend/resume lifecycle; the new driver adds that
per-operation guard.

### C3: Locking and multicore

The critical section uses `esp_sync::RawMutex` (`lib.rs:48-58`). On multi-core
chips this is a cross-core spinlock plus local interrupt disable. It does not
stop the other core from executing from flash.

`MultiCoreStrategy` is in `common.rs:229-258`. Its hooks guard write and erase,
not read. `AutoPark` uses `CpuControl::park_core`, which applies the RTC
hardware stall. `esp-storage` parks before taking the flash lock
(`common.rs:282-289`); the new driver reverses that order.

### C4: Encrypted access and MMU

The local `esp_rom_spiflash_write_encrypted` declaration requires 32-byte
address and length alignment (`ll.rs:78-80`,
`esp-rom-sys/src/rom/spiflash.rs:36-38`).

`esp-storage/src/encrypted.rs:44-79` wraps it in a 4096-byte sector
read-modify-write. That wrapper behavior is not carried into the new driver.

ESP-IDF's public `esp_flash_write_encrypted` contract requires erased flash and
16-byte-aligned address and length. `esp_partition_write` forwards encrypted
partitions to it without adding read-modify-write.

The operation returns `NotSupported` when encryption is off. Temporary
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
