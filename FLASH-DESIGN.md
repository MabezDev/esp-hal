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

## Compatibility requirements

The new driver must preserve the behavior on which existing users depend,
except where this document calls out a deliberate change:

- Standard read, write, and erase operations call dedicated
  `esp_rom_spiflash_*` ROM functions.
- ESP32 uses the static ROM patch library supplied by `esp-rom-sys`.
- Write and erase are split into page, sector, or block calls.
- Each ROM call has its own operation guard. Interrupts run between calls.
- The operation guard suspends and restores the cache around the low-level ROM
  function. The ROM function does not own that lifecycle.
- Write and erase invalidate affected cache mappings before returning.
- Encrypted writes deliberately change to the ESP-IDF contract: the caller
  erases first, and the driver does not perform read-modify-write.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, BuilderLite)]
#[non_exhaustive]
pub struct Config {
    #[cfg(multi_core)]
    #[builder_lite(unstable)]
    multi_core_strategy: MultiCoreStrategy,
}
```

The field starts unstable. The stable `Config` surface is therefore empty.
On single-core chips the whole surface is empty. `#[non_exhaustive]` leaves
room for proven needs.

`multi_core_strategy` defaults to `AutoPark`. BuilderLite provides the unstable
`with_multi_core_strategy(...)` setter. There is no capacity or chip-parameter
escape hatch. One can be designed later from a demonstrated hardware need.

Configuration is applied only during construction. There is no
`apply_config()` method because the design has no justified runtime setting.

### Detection

Construction reads the 24-bit JEDEC ID cached by the first-stage ROM
bootloader:

1. obtain `g_rom_flashchip.device_id` through a uniform `esp-rom-sys`
   accessor;
2. reject all-zero and all-one IDs;
3. decode physical capacity;
4. panic if the cached ID is invalid or has an unsupported density encoding.

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

This driver supports only the fixed internal boot flash from which the ROM has
already loaded the running image. It therefore uses the cached ID uniformly
instead of reinitializing an ID transaction or poking SPI1 registers. This
also incorporates any vendor startup correction that updated the ROM cache,
such as ESP-IDF's XMC path.

The returned ID is decoded with the esptool-derived capacity table. Capacity
zero is never a valid driver state. There is no capacity fallback: an
unsupported density encoding violates the internal-boot-flash assumption and
panics. A demonstrated chip can motivate explicit support later.

The driver does not use cached `g_rom_flashchip.chip_size` as physical
capacity. A second-stage bootloader may replace that field with the size from
the binary image header. Decoding `device_id` preserves physical-capacity
semantics.

ESP-IDF separately compares physical capacity with the size configured in the
binary image header. It fails when physical capacity is smaller and limits
available capacity when physical capacity is larger. This driver deliberately
does not copy that configured-size policy: it is bootloader-independent and
reports detected physical capacity, as `esp-storage` does today.

`chip_info()` reports the chip ID, capacity, and fixed geometry:

```rust
#[instability::unstable]
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

`chip_id` always reports the detected ID. `capacity` reports the detected
physical capacity.

### Construction invariant

Construction is logically infallible because the first-stage ROM has already
loaded the running image from this fixed flash chip. There is currently no
configuration error that the caller can remedy, but the API follows the
esp-hal driver guideline and retains an empty non-exhaustive error:

```rust
#[non_exhaustive]
pub enum ConfigError {}
```

`new()` nevertheless asserts that the cached ID is neither `0x000000` nor
`0xFFFFFF` and that its density encoding is supported. Violating either
condition panics rather than creating the old unusable capacity-zero state.
These panic conditions must be documented on the constructor.

## Public API

The driver owns the `FLASH` singleton, capacity, unlock state, and mode marker.
Working storage is static internal RAM rather than part of the movable driver
value.

```rust
pub struct Flash<'d, Dm: DriverMode> { /* private fields */ }

impl<'d> Flash<'d, Blocking> {
    #[ram]
    pub fn new(flash: FLASH<'d>, config: Config)
        -> Result<Self, ConfigError>;

    #[ram]
    pub fn read(&mut self, offset: u32, data: &mut [u8])
        -> Result<(), Error>;
    #[ram]
    pub fn write(&mut self, offset: u32, data: &[u8])
        -> Result<(), Error>;
    #[ram]
    pub fn erase(&mut self, from: u32, to: u32)
        -> Result<(), Error>;
    #[ram]
    pub fn capacity(&self) -> usize;

    #[ram]
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

One chunk is one bounded ROM call. A read or write chunk is at most 256 bytes.
An erase chunk is one sector or block.

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

The bounce buffer is one private 256-byte static in internal RAM. The `FLASH`
singleton and exclusive `&mut Flash` access prevent concurrent use. Moving the
`Flash` value into PSRAM does not move this buffer.

The 256-byte size is an initial policy, not a stable guarantee. A later design
may make the size or storage configurable when a real user needs to trade
internal RAM for fewer ROM calls. Any such option must preserve internal-RAM
residency and exclusive access while the cache is disabled.

Every public method and the code it needs during a flash operation is marked
with `#[ram]`. This is an implementation rule, not a stable user-facing
guarantee. Linked-section checks must verify it.

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

The local `esp_rom_spiflash_write_encrypted` binding documents 32-byte
alignment, while ESP-IDF's public API promises 16-byte alignment. The
implementation must resolve and test that mismatch while preserving the
16-byte public contract.

Encrypted reads and writes use the same private 256-byte maximum chunk as
plain access. Encrypted reads map and copy at most one chunk at a time.
Encrypted writes split larger aligned inputs into 256-byte ROM calls.

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

    #[ram]
    fn read(&mut self, offset: u32, data: &mut [u8])
        -> Result<(), Self::Error> { /* ... */ }

    #[ram]
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

The legacy `ReadStorage` and `Storage` traits are not implemented. Callers that
need a plain sub-sector update must perform the read-modify-write themselves.

The dependency uses the version-suffixed name `embedded-storage-03`. It is
optional and enabled by `unstable`; no new cargo feature is added.
Stabilization decides whether the blocking traits can stabilize on 0.3 or must
wait for 1.0. See [A9](#a9-embedded-storage-trait-stabilization-deferred).

## Private internals: `rom.rs` and `mmu.rs`

`rom.rs` owns:

- thin wrappers around the `esp_rom_spiflash_*` functions;
- private access to the ROM-cached flash ID;
- bounds checks and chunk loops;
- lock, park, and unpark ordering;
- direct and bounce-buffer routing;
- the static 256-byte internal-RAM bounce buffer.

`mmu.rs` owns temporary mappings for encrypted reads and the required cache
invalidation, including the P4 variant.

Neither module is public. `esp-rom-sys` is the raw escape hatch. Every public
method and its required call path is marked `#[ram]`. Every operation resolves
to a dedicated ROM function or a real `NotSupported` environmental case. There
is no partial dispatch, `unreachable!()`, or panic arm.

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
- Verify detection rejects all-zero and all-one cached IDs and unsupported
  density encodings.
- Validate that the first-stage ROM populates the cached 24-bit ID and that
  decoded capacity matches the board on every supported chip family,
  including octal-flash boot modes.
- Test direct and bounced reads across 256-byte chunk boundaries.
- Test flash-resident and PSRAM buffers where PSRAM exists.
- Verify every public method and required helper is linked into a RAM section.
- Test encrypted reads in the encryption-off mode used by the CI fleet.
- Exercise encrypted writes through an explicit test-only bypass of the
  production encryption guard, and separately verify that the production API
  returns `NotSupported` when encryption is off.
- Verify that encrypted writes reject unaligned ranges, never erase
  implicitly, and operate on previously erased 16-byte-aligned ranges.
- Resolve the ROM binding's 32-byte claim against the 16-byte ESP-IDF contract
  on each implementation family.
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

The boot flash uses standard ROM commands and the ID cached by the first-stage
ROM. Direct register execution would add RAM-resident bit manipulation for no
current operation. The driver calls ROM functions only. Unsupported future
commands may return `NotSupported`.

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

The ROM has already booted from the internal flash with its default commands.
General command and geometry overrides serve external chips, not the internal
driver. There is no capacity escape hatch. Invalid or unsupported cached
identification violates the platform invariant and triggers the documented
construction panic. Any future exception must start from a demonstrated chip
and specify more than an assumed capacity when its behavior differs from the
standard ROM operations.

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

### A18: Public flash methods live in RAM

Every public method and its required call path is placed in RAM. This is an
implementation rule verified from linked sections, not a stable placement
guarantee. The driver value may move because working storage is a static
internal-RAM buffer. PSRAM-backed stacks remain unsupported.

### A19: Encrypted writes match ESP-IDF

`write_encrypted` programs previously erased flash at 16-byte-aligned addresses
and lengths. It does not erase or perform sector read-modify-write. A separate
overwrite helper remains deferred. Hardware validation must reconcile the
local ROM binding's 32-byte alignment claim with the 16-byte public contract.

## Appendix B: ROM capability evidence

### B1: Dedicated ROM functions

Read, write, sector erase, block erase, unlock, and encrypted write have
dedicated functions on all supported chips. ESP32's incomplete ROM is patched
by `esp-rom-sys/libs/esp32/libesp_rom.a`, built from the IDF v5.3.1 ROM patch.
Whole-chip erase evidence is kept with that deferred API in
`FLASH-DEFERRED.md`.

### B2: Three-byte RDID

ESP-IDF v6.0.2 implements `memspi_host_read_id_hs()` in
`components/spi_flash/memspi_host_driver.c`. It sends `CMD_RDID` through
`common_command` with `miso_len = 3`, rejects `0x000000` and `0xFFFFFF`, and
normalizes the byte order. `esp_flash_read_chip_id()` in
`components/spi_flash/esp_flash_api.c` reads twice and rejects a mismatch;
`esp_flash_init()` and `esp_flash_init_main()` retry that transient result.

`esp_flash_init_main()` bypasses ordinary RDID in octal mode and uses
`g_rom_flashchip.device_id`. The cross-target ROM declaration is direct on
ESP32 and ESP32-S2 and is reached through `rom_spiflash_legacy_data` on newer
chips.

`esp_rom_spiflash_read_user_cmd(status: *mut u32, cmd: u8)` exists on all
supported chips, but its ROM implementations configure and return only one
response byte. It can read a status-like byte, not the three-byte JEDEC ID.

The first-stage ROM cache is a better fit for an internal-only driver. It
already identifies the chip that supplied the running image, works when
ordinary RDID is inappropriate in octal mode, and avoids depending on
ESP-IDF's application-initialized host context. PR A adds a uniform accessor
in `esp-rom-sys` and validates the cache across the supported chip families.

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
chunks. This critical section does not provide ESP-IDF's explicit cache
suspend/resume lifecycle; the new driver adds that per-operation guard.

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
