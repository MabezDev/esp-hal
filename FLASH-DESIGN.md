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
| embedded-storage trait implementations | | decision required at stabilization |
| `MultiCoreStrategy`, `OtherCoreRunning` | | yes |

The `Dm` parameter is part of the initial API. esp-hal mode parameters have no
default, so adding it after stabilization would break every explicit `Flash`
type. Only `Blocking` is constructible in this design. A possible async API is
deferred without removing the type-state parameter.

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
  function. The ROM function does not own that lifecycle.
- Write and erase invalidate affected cache mappings before returning.
- Encrypted writes deliberately change to the ESP-IDF contract: the caller
  erases first, and the driver never erases implicitly or changes bytes outside
  the requested range. It is not `esp-storage`'s sector read-modify-write. On
  ESP32 a 16-byte-aligned row edge still needs a bounded, row-local decrypt and
  re-encrypt of the neighbouring block; see
  [B5](#b5-esp-idfs-encrypted-write-row-handling).
- Encrypted writes fail with `NotSupported` when flash encryption is off.
- Encrypted reads keep the private MMU path, including P4 cache invalidation.
- The bootloader's host tests remain possible after they stop depending on the
  concrete hardware type.

The new driver also fixes two unsafe buffer assumptions in `esp-storage`.
Flash-resident write data and PSRAM-resident buffers cannot be used directly
while the cache is off. The new driver stages those buffers through internal
RAM using one static 256-byte bounce buffer.

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

Construction reads the 24-bit JEDEC ID cached in the ROM flash-chip structure:

1. obtain `device_id` through a uniform `esp-rom-sys` accessor;
2. reject known no-response sentinels;
3. decode physical capacity from the manufacturer and density bytes;
4. return `ConfigError::UnknownFlashChip` if the cached ID is a sentinel or its
   density encoding is unsupported.

ESP-IDF obtains the full ID through `memspi_host_read_id_hs()`. That function
sends `CMD_RDID` through the initialized MSPI host's `common_command` path with
a three-byte MISO phase. It does not use
`esp_rom_spiflash_read_user_cmd()`, which reads only one response byte and
cannot supply the density byte.

ESP-IDF normally reads twice, rejects a mismatch, and retries transient
results while initializing a standard-mode chip. Its initialized MSPI host
context is not available to an esp-hal application. More importantly, ESP-IDF
itself bypasses ordinary RDID and uses `g_rom_flashchip.device_id` when the
boot flash is already in octal mode.

This driver supports only the fixed internal boot flash from which the boot
chain has already loaded the running image. It therefore uses the cached ID
uniformly instead of reinitializing an ID transaction or poking SPI1 registers.
This also incorporates any vendor startup correction that rewrote the cache,
such as ESP-IDF's XMC path.

#### Who populates the cache

On every supported chip except ESP32, the **first-stage ROM bootloader**
populates `device_id`. `ets_run_flash_bootloader` calls the ROM's own
`esp_rom_spi_flash_update_id`, which issues `RDID` over SPI1, byte-swaps the
response, and stores it. The second-stage bootloader then repeats the same work
defensively. On ESP32 that ROM function does not exist, so the second-stage
bootloader is the only writer. See
[B2](#b2-the-cached-jedec-id-and-its-provenance) for the disassembly.

The ROM ships the structure statically initialized to a Winbond W25Q16
descriptor: `device_id` `0x001540EF`, `chip_size` 2 MiB, 64 KiB blocks, 4096-byte
sectors, 256-byte pages. That is what is present before any detection runs.

Three consequences shape the API.

First, the driver can refresh the cache itself rather than trusting the boot
chain. `esp_rom_spi_flash_update_id` is a ROM function, already provided by the
`esp-rom-sys` linker scripts on all nine targets that have it, so calling it
keeps [A1](#a1-no-spi1-register-exec-fallback)'s ROM-functions-only rule intact.
Detection should call it before reading `device_id` on those targets. It touches
SPI1, so it belongs inside the operation guard like any other flash access.

Second, ESP32 has no such function, so it depends on an ESP-IDF-compatible
second-stage bootloader. That is an accepted, documented platform requirement;
see [A13](#a13-no-chip-configuration-at-launch). `esp-storage` reaches the same
conclusion from the other direction: its comment says the hardware `RDID` path is
unreliable on ESP32, and it reads the cached `chip_size` there instead of
detecting.

Third, the failure mode is a plausible wrong answer, not an obvious one. An
uninitialized cache reads as a real W25Q16 ID and a 2 MiB capacity, which no
all-zero or all-ones sentinel check would catch. Detection must reject the ROM
default value explicitly. It happens to fail the density check anyway, but only
by accident; see [Byte order](#byte-order).

Construction is therefore fallible rather than invariant. See
[A13](#a13-no-chip-configuration-at-launch).

The reason to prefer `device_id` over `chip_size` is unchanged: `device_id`
carries the result of a real hardware `RDID`, while `chip_size` carries the size
configured in the binary image header.

#### Byte order

`device_id` is stored most-significant-byte first: manufacturer in bits 23:16,
memory type in bits 15:8, density in bits 7:0. The ROM's
`esp_rom_spi_flash_update_id`, the second-stage bootloader's
`bootloader_read_flash_id()`, and ESP-IDF's `memspi_host_read_id_hs()` all
byte-swap the raw `RDID` response to reach that same layout.

The ROM's *static default* is the exception: `0x001540EF` is a W25Q16 ID in raw
order, not manufacturer-first. Espressif is inconsistent here, and it works in
our favour. Under the manufacturer-first decode the default's density byte is
`0xEF`, which is not a valid density, so an uninitialized cache fails detection
instead of silently reporting 2 MiB. Do not rely on that accident; reject the
value explicitly.

The esptool capacity table and `esp-storage`'s current decoder both consume the
*raw* `RDID` order, where the manufacturer sits in bits 7:0 and the density in
bits 23:16. Porting the table unchanged onto `device_id` therefore reads the
wrong byte. The decoder must extract:

| Field | Raw `RDID` order (esptool, `esp-storage`) | Cached `device_id` order |
|-------|------------------------------------------|--------------------------|
| Manufacturer | bits 7:0 | bits 23:16 |
| Density, standard vendors | bits 23:16 | bits 7:0 |
| Density, Adesto (`0x1F`) | bits 12:8 | bits 12:8 |

Only the Adesto middle-byte case is order-independent. This asymmetry is the
easiest way to get detection silently wrong, so the HIL test asserts the
decoded capacity against the known board, not merely that decoding succeeded.

#### Capacity policy

Capacity zero is never a valid driver state. There is no capacity fallback and
no capacity escape hatch: an unsupported density encoding is reported as a
construction error, and a demonstrated chip can motivate explicit support
later.

The driver does not use cached `chip_size` as physical capacity, because the
second-stage bootloader overwrites that field with the image-header size.

ESP-IDF separately compares physical capacity with the size configured in the
binary image header. It fails when physical capacity is smaller and limits
available capacity when physical capacity is larger. This driver deliberately
does not copy that configured-size policy: it reports detected physical
capacity, as `esp-storage` does today.

`chip_info()` reports the chip ID, capacity, and fixed geometry:

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

- page size: 256 bytes;
- sector size: 4096 bytes;
- block size: 64 KiB.

`chip_id` always reports the detected ID. Because two byte orders are in play,
the documentation must state which one this is: manufacturer-first, matching the
cached value and ESP-IDF's `chip_id`, so `0xC86016` is GigaDevice. `capacity`
reports the detected physical capacity.

### Construction errors

Construction fails when the cached identification cannot be decoded:

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

This replaces an earlier design that panicked instead. `new()` already returns
`Result`, the developer guidelines prefer a fallible API over a panic, and the
failure is reachable rather than hypothetical because the cache is populated by
the bootloader rather than by the ROM. A caller that genuinely wants a panic can
`unwrap()`; a bootloader or recovery tool can fall back to its own detection.

Capacity zero is never constructed. Either detection succeeds or `new()`
returns `Err`, so no operation ever fails bounds checks against a zero
capacity, which is the failure mode `esp-storage` has today.

The variant is deliberately coarse. It says the platform assumption did not
hold, not which byte was wrong; the raw cached value is logged before the error
is returned.

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

    pub fn read(&mut self, offset: u32, data: &mut [u8])
        -> Result<(), Error>;
    pub fn write(&mut self, offset: u32, data: &[u8])
        -> Result<(), Error>;
    pub fn erase(&mut self, from: u32, to: u32)
        -> Result<(), Error>;
    pub fn capacity(&self) -> usize;

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

`#[ram]` does not appear on these methods. It belongs on the operation guard and
the ROM wrappers underneath them, as described in
[RAM residency](#ram-residency).

There is no `into_async()` method or async implementation in the current
design. Keeping the `Dm` parameter now allows one to be added later without
breaking explicit `Flash` type annotations.

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
return code and the ROM call that produced it.

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
core from holding the flash lock.

One chunk is one bounded ROM call. An erase chunk is one sector or block.

### Chunk size

Chunk size is a throughput decision, not a buffer-management detail, so the two
paths do not share a limit:

| Path | Chunk limit | Why |
|------|-------------|-----|
| Direct, buffer already in internal RAM and aligned | 4096 bytes | matches `esp-storage`; no staging needed |
| Staged through the bounce buffer | 256 bytes | the bounce buffer is the limit |

An earlier revision used 256 bytes everywhere. That is 16 times smaller than
`esp-storage`, which chunks reads and writes by sector, and 32 to 64 times
smaller than ESP-IDF, which uses `MAX_WRITE_CHUNK` of 8192 and
`MAX_READ_CHUNK` of 16384. Every chunk boundary pays a full guard: take the
lock, park the other core, disable interrupts, suspend cache, call ROM,
invalidate, resume, unpark, release. Paying that 16 times more often on the
path that needs no staging at all is a regression against the driver this one
replaces, and throughput is visible through the stable API.

Tying the direct path to the bounce buffer also made the two questions
inseparable: [`FLASH-DEFERRED.md`](FLASH-DEFERRED.md#9-configurable-bounce-storage)
could not revisit buffer size without changing the chunking of buffers that are
never staged.

Both limits are private implementation policy. Neither is a stable guarantee,
and both should be confirmed by measurement in PR A rather than assumed.

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

The bounce buffer is one private 256-byte static in internal RAM. The `FLASH`
singleton and exclusive `&mut Flash` access prevent concurrent use. Moving the
`Flash` value into PSRAM does not move this buffer.

The 256-byte size is an initial policy, not a stable guarantee. A later design
may make the size or storage configurable when a real user needs to trade
internal RAM for fewer ROM calls. Any such option must preserve internal-RAM
residency and exclusive access while the cache is disabled.

`READ_SIZE` is 1, so a read may start mid-word. The ROM read primitive requires
a word-aligned offset and length, so an unaligned read stages an aligned
superset through the bounce buffer and copies out the requested window. The
usable payload of a staged chunk is therefore up to three bytes less than the
buffer. Chunk arithmetic must account for that rather than assuming a full
buffer per chunk.

### RAM residency

`#[ram]` covers the code that runs while the cache is suspended: the operation
guard, the ROM wrappers it calls, the cache and interrupt manipulation, and the
mapping invalidation. It deliberately does not cover every public method.

Bounds checks, alignment checks, the chunk loop, and buffer-placement routing
all run with the cache enabled and are free to execute from flash. `esp-storage`
draws the line in the same place: `#[ram]` sits on the ROM wrappers in
`hardware.rs`, which contain the critical section and the ROM call, and not on
the public methods or trait implementations. Internal RAM is a scarce, shared
resource, so pulling the whole public surface into it costs real memory for no
correctness gain.

This is an implementation rule, not a stable user-facing guarantee. The
linked-section check verifies that the guard-and-ROM-call set is RAM-resident,
which means the set has to be named somewhere the test can assert against.

The caller's stack must still be in internal RAM. ROM code uses that stack
while the cache is off. PSRAM-backed stacks are unsupported by documentation;
the driver does not check them at runtime.

### Whole-chip erase

The current driver deliberately has no dedicated whole-chip erase API. The ROM
operation is one unchunked call that keeps interrupts disabled, may keep the
other core parked, cannot feed a watchdog, and erases the code that invoked it.
It is useful mainly to RAM-resident flasher stubs, which need a stronger
execution contract than this general driver currently expresses.

The candidate API, target-specific ROM evidence, and destructive validation
work are deferred to
[`FLASH-DEFERRED.md`](FLASH-DEFERRED.md#7-dedicated-whole-chip-erase).

## Encrypted access

`read_encrypted()` maps flash pages through the private MMU path so reads are
decrypted by hardware. P4 must invalidate both L1 data cache and L2 after
changing the mapping. When flash encryption is off, the read returns the
plaintext bytes.

`write_encrypted()` follows ESP-IDF's `esp_flash_write_encrypted()` contract:

- the destination must already be erased;
- address and length must be multiples of 16 bytes;
- the driver does not erase or preserve surrounding bytes;
- writing an external flash chip is not supported.

This removes `esp-storage`'s implicit 4096-byte sector read-modify-write. A
separate sector-overwrite helper can be added later without changing this API.
That deferred API is recorded in
[`FLASH-DEFERRED.md`](FLASH-DEFERRED.md).

`esp-bootloader-esp-idf` exposes two direct, checked conversions from a
partition entry:

```text
PartitionEntry::as_flash_region()           -> FlashRegion
PartitionEntry::as_encrypted_flash_region() -> EncryptedFlashRegion
```

`FlashRegion` is guaranteed to be effectively plaintext and implements the
blocking NOR traits directly. `EncryptedFlashRegion` is guaranteed to be
effectively encrypted and exposes inherent `read`, `erase`, and encrypted
write methods. It does not implement `NorFlash` or `MultiwriteNorFlash`,
because physical erase does not read back as plaintext `0xFF` through the
decryption view.

Both accessors check effective encryption using the partition flags, partition
type, and hardware encryption state. Higher-level bootloader flows such as OTA
own that policy choice and select the correct accessor. Any enum needed to
hide the runtime choice remains private to the bootloader crate. There is no
intermediate dynamic `FlashRegion` followed by `NorFlashRegion` or
`EncryptedNorFlashRegion`.

Encrypted writes require prior erase plus 16-byte alignment. Byte-granular
encrypted overwrite is not preserved.

### The 16-byte contract costs work on ESP32

The `esp_rom_spiflash_write_encrypted` binding documents 32-byte alignment and
ESP-IDF's public API promises 16-byte alignment. Both are correct, and the gap
is bridged by ESP-IDF rather than left open. See
[B5](#b5-esp-idfs-encrypted-write-row-handling).

The ROM row size differs by target. ESP32 encrypts a 32-byte row as two AES
blocks that share a key derived from the flash address. ESP32-S2 and later
accept 16, 32, or 64-byte rows directly, so a 16-byte-aligned request lowers
straight onto the ROM call.

On ESP32 a request that starts or ends on a 16-byte but not 32-byte boundary
needs the other half of the row. ESP-IDF reads that half back *decrypted*, then
re-encrypts it unchanged as part of the full 32-byte row. There is no way around
this: physical flash holds ciphertext, so the untouched 16 bytes can only be
preserved by decrypting and re-encrypting them.

That is a read-modify-write, which the section above says the driver does not
do. The contradiction is real and has to be settled rather than tested:

- **Match ESP-IDF.** Accept 16-byte alignment everywhere and implement the
  pre/post decrypted read-back on ESP32. The API is uniform and portable code
  behaves the same on every chip. The cost is that ESP32 carries a 16-byte
  read-modify-write inside a method documented as not doing one, and that
  read-back only round-trips correctly when encryption is actually enabled.
- **Expose the hardware.** Require 32-byte alignment on ESP32 and 16 elsewhere,
  as a documented per-chip constant. No hidden read-modify-write, but the
  contract is no longer uniform and callers need a `cfg`.

Matching ESP-IDF is the recommendation, because the dominant consumer is the
bootloader's OTA path and a per-chip alignment constant would leak into it. The
prohibition on read-modify-write should then be narrowed to say what it is
actually protecting: the driver never erases implicitly, and never touches bytes
outside the requested range other than to preserve them. Either way this is a
decision for PR C, not a HIL assertion.

### Encrypted chunking

Encrypted reads map and copy at most one page-limited chunk at a time.
Encrypted writes split larger aligned inputs along the same direct and staged
limits as plain access, respecting the row size above.

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
for reads as well. A future cooperative strategy may wait for the other core
to reach a safe point, but it is not part of this design.

Reads cannot rely on SPI1/cache arbitration while calling the low-level ROM
primitive. ESP-IDF wraps reads in the same cache and other-core guard as writes
and erases. Stabilization needs a dual-core hardware test that confirms
`AutoPark` prevents XIP execution on the other core during each read chunk and
restores it afterward.

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

    fn read(&mut self, offset: u32, data: &mut [u8])
        -> Result<(), Self::Error> { /* ... */ }

    fn capacity(&self) -> usize { /* ... */ }
}

#[instability::unstable]
impl NorFlash for Flash<'_, Blocking> {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = 4096;

    fn erase(&mut self, from: u32, to: u32)
        -> Result<(), Self::Error> { /* ... */ }

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
optional and enabled by `unstable`; no new cargo feature is added.
Stabilization decides whether the blocking traits can stabilize on 0.3 or must
wait for 1.0. See [A9](#a9-embedded-storage-trait-stabilization-deferred).

## Private internals: `rom.rs` and `mmu.rs`

`rom.rs` owns:

- thin `#[ram]` wrappers around the `esp_rom_spiflash_*` functions;
- the `#[ram]` operation guard: cache suspend and resume, interrupt disable and
  restore, park and unpark, mapping invalidation;
- private access to the ROM-cached flash ID and its byte-order normalization;
- bounds checks and chunk loops;
- lock, park, and unpark ordering;
- direct and bounce-buffer routing;
- the static 256-byte internal-RAM bounce buffer.

Only the first two items are RAM-resident. The rest runs with the cache enabled.

`mmu.rs` owns temporary mappings for encrypted reads and the required cache
invalidation, including the P4 variant.

Neither module is public. `esp-rom-sys` is the raw escape hatch. Every operation
resolves to a dedicated ROM function or a real `NotSupported` environmental
case. There is no partial dispatch, `unreachable!()`, or panic arm.

## Host tests and migration

esp-hal does not build for the host. The driver therefore cannot copy
`esp-storage`'s emulation backend.

The bootloader host tests must depend on an internal flash abstraction and a
test mock, not on `Flash` itself. Hardware migration happens after that seam is
in place.

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
- Verify the guard and ROM-wrapper set is linked into a RAM section, and that
  public methods are *not* pulled in with it.
- Measure read and write throughput against `esp-storage` on one RISC-V and one
  Xtensa target. A large regression means the chunk policy is wrong.
- Test encrypted reads in the encryption-off mode used by the CI fleet.
- Exercise encrypted writes through an explicit test-only bypass of the
  production encryption guard, and separately verify that the production API
  returns `NotSupported` when encryption is off.
- Verify that encrypted writes reject unaligned ranges, never erase
  implicitly, and operate on previously erased ranges at the alignment the
  chosen contract promises.
- On ESP32, exercise a 16-byte-aligned but not 32-byte-aligned encrypted write
  at both the start and the end of a range, which is the case that needs the
  pre/post decrypted read-back.
- Record that a true encrypted round trip still needs a board with encryption
  eFuses set.
- Build the bootloader and examples through the new API.
- Run bootloader host tests through the mock abstraction.
- Update the semver baseline before stabilization.

Decisions that must be closed before the gates above are meaningful:

- the encrypted-write alignment contract, uniform 16 bytes or per-chip
  ([B5](#b5-esp-idfs-encrypted-write-row-handling));
- the direct-path chunk limit, once PR A has throughput numbers
  ([Chunk size](#chunk-size));
- the embedded-storage version question
  ([A9](#a9-embedded-storage-trait-stabilization-deferred)).

## Appendix A: Decision log

This appendix records the final result of each reviewed design question.
Superseded material that may still be useful lives in
[`FLASH-DEFERRED.md`](FLASH-DEFERRED.md).

### A1: No SPI1 register-exec fallback

The boot flash uses standard ROM commands and the cached ID. Direct register
execution would add RAM-resident bit manipulation for no current operation. The
driver calls ROM functions only. Unsupported future commands may return
`NotSupported`.

`esp-storage` reads `RDID` from SPI1 registers itself. The driver does not need
to copy that, because the ROM exports `esp_rom_spi_flash_update_id` on nine of
the ten targets and `esp-rom-sys` already provides the symbol. That function
performs the `RDID`, byte-swaps the response, handles the high-speed dummy-length
case, and stores the result into the cache. Calling it gives the driver
`esp-storage`'s bootloader independence through a ROM function rather than
hand-rolled register manipulation, so this rule costs nothing on those targets.
ESP32 is the exception and keeps the bootloader dependency. See
[B2](#b2-the-cached-jedec-id-and-its-provenance).

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
unconditional CPU stall around every low-level ROM operation. Reads use the
same cache and other-core guard because ESP-IDF does not treat the ROM read
primitive as safe alongside XIP cache activity.

### A5: `mmap` would be an inherent method

The original extension trait had one implementor and added no value. A future
mapping API should use inherent methods. [A16](#a16-public-mmap-cut-mmu-machinery-stays-private)
later removed mapping from the current surface.

### A6: Error type shape

The public type is `flash::Error`. `Unknown` follows esp-hal naming and has no
payload. Unexpected ROM codes are logged. A future external driver owns its
own error type.

### A7: Dedicated whole-chip erase is deferred

A dedicated whole-chip ROM call is deliberately absent. It cannot be chunked,
keeps the system unavailable for an unbounded period, and destroys the running
image. Its useful callers are RAM-resident tools that need an execution-safety
contract the current driver does not express. The candidate API and ROM
evidence live in `FLASH-DEFERRED.md`.

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

The boot chain has already booted from the internal flash with its default
commands. General command and geometry overrides serve external chips, not the
internal driver. There is no capacity escape hatch. Any future exception must
start from a demonstrated chip and specify more than an assumed capacity when
its behavior differs from the standard ROM operations.

Invalid or unsupported cached identification returns
`ConfigError::UnknownFlashChip` rather than panicking.

The original design panicked on the grounds that the first-stage ROM guarantees a
valid cache. That premise turns out to be correct on nine of the ten targets:
`ets_run_flash_bootloader` calls the ROM's own `esp_rom_spi_flash_update_id`
([B2](#b2-the-cached-jedec-id-and-its-provenance)). It does not hold on ESP32,
where the ROM has no such function and the second-stage bootloader is the only
writer.

An error is still the right choice for three reasons. The developer guidelines
prefer a fallible API over a panic when a `Result` is already being returned.
ESP32 leaves a real, if narrow, failure path. And the value present before
detection is not an obvious sentinel but a hardcoded W25Q16 descriptor reporting
2 MiB, so a driver that assumed success could serve a wrong capacity rather than
fail loudly. `unwrap()` remains available to any caller who wants the panic.

**ESP32 requires an ESP-IDF-compatible second-stage bootloader.** This is a
settled platform requirement, not an open question. ESP32's ROM has no
identification function, the ROM patch library never had one to patch in, and the
remaining routes are either single-byte or register-level
([B2](#b2-the-cached-jedec-id-and-its-provenance)). Since any espflash-flashed
image carries that bootloader, and ESP32 is old enough that new bootloaders for
it are unlikely, documenting the requirement is a better trade than carrying a
chip-specific detection path.

The requirement must appear in the module's `Limitations` block and on `new()`,
alongside the note that a boot chain which does not populate the cache yields
`ConfigError::UnknownFlashChip`. If that ever stops being acceptable, exporting
the ROM's `SPI_Common_Command` on ESP32 is the escalation, because it stays
within [A1](#a1-no-spi1-register-exec-fallback).

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

### A18: RAM residency follows the cache-off window

`#[ram]` is scoped to the operation guard and the ROM wrappers, which is the
code that runs while the cache is suspended. An earlier revision placed every
public method and its whole call path in RAM. That is a superset of what
correctness needs, and internal RAM is scarce: bounds checks, the chunk loop,
and buffer routing all run with the cache on and can execute from flash.
`esp-storage` draws the same line.

This is an implementation rule verified from linked sections, not a stable
placement guarantee. The driver value may move because working storage is a
static internal-RAM buffer. PSRAM-backed stacks remain unsupported.

### A19: Encrypted writes match ESP-IDF

`write_encrypted` programs previously erased flash and never erases implicitly.
A separate sector-overwrite helper remains deferred.

The alignment contract is not yet settled. The ROM row size is 32 bytes on
ESP32 and 16, 32 or 64 bytes on later chips, and ESP-IDF reaches a uniform
16-byte public contract by reading back the adjacent 16-byte block decrypted and
re-encrypting it unchanged
([B5](#b5-esp-idfs-encrypted-write-row-handling)). Matching ESP-IDF is the
recommendation, which means accepting a bounded read-modify-write on ESP32 and
narrowing the "no read-modify-write" rule to "no implicit erase, and no change
to bytes outside the requested range". The alternative is a per-chip alignment
constant. PR C owns the choice.

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
