# `esp_hal::flash`: Deferred material

This annex holds ideas that are outside the current internal flash driver.
[`FLASH-DESIGN.md`](FLASH-DESIGN.md) remains the authority for committed
behavior.

Nothing here is planned work. Each section has an activation condition, and
all evidence must be checked again before implementation.

## 1. External SPI NOR flash driver

### Boundary

External flash is a separate type. It may run on SPI2/SPI3 or on a portable
bus abstraction. It shares embedded-storage traits with
`esp_hal::flash::Flash`, but it does not share the internal driver's
configuration, errors, encryption, MMU support, or multi-core policy.

This split is fixed by
[design A12](FLASH-DESIGN.md#a12-internal-only-scope-the-external-backend-splits-out).
The rest of this section remains open.

### Two possible bus designs

These are different drivers or front ends, not interchangeable constructor
choices:

| | Portable, embedded-hal `SpiDevice` | esp-hal-specific SPI type |
|-|-----------------------------------|---------------------------|
| Bus sharing | yes, and `embedded-hal-bus` supplies shared-bus implementations, though it does not define the trait | needs an explicit bus-ownership design |
| Home | could live outside esp-hal | inside esp-hal |
| Half-duplex command, address, dummy cycles | not exposed | native |
| Hardware chip select | no | yes |
| DMA | cannot assume | yes, and owns DMA errors |
| Dual and Quad I/O | no | yes |
| Errors owned | SPI | SPI and DMA |
| Async | blocking or embedded-hal async transactions | interrupt-driven DMA completion with a bound handler, so `Async: !Send` has a real hardware reason |

### Behavior worth keeping

Both designs lower each operation to a command transaction. Program and erase
completion use status-register polling with a timeout, not a fixed delay. Chip
geometry, commands, status bits and timeouts come from a chip description.

### Open choices

- Type name and crate or module home.
- Portable `SpiDevice`, esp-hal SPI, or separate support for both.
- Owned bus versus shared-device ownership.
- Trait constants when runtime chip geometry varies.
- P4 qualification for the chosen SPI and DMA path.

### External-driver prior art

`qa-test/src/bin/qspi_flash.rs` drives a GD25Q64C on SPI2 with raw half-duplex
commands. It proves the esp-hal transaction shape and hardware path. Its fixed
waits are test shortcuts, not a design to copy.

The test still excludes P4, but that filter does not prove a driver limit. P4
metadata assigns SPI2/SPI3 to AXI-GDMA, and the SPI master supports that
engine. P4 needs qualification when this test or driver is implemented.

### Activation condition

Start this work when a committed implementer is ready to choose and own the bus
API.

## 2. `ChipConfig`: chip parameter vocabulary

`ChipConfig` is a possible data description for external flash and for any
future internal override. It is not part of the current internal driver.

The useful field groups are:

| Group | Values |
|-------|--------|
| Identity | optional capacity |
| Geometry | page, sector, and block sizes |
| Commands | read, program, sector erase, block erase, chip erase, write enable, read status |
| Status | busy-bit mask |
| Timing | program, sector erase, block erase, and chip erase timeouts |

### Defaults

The default must describe a standard 25-series SPI NOR chip:

- 256-byte pages;
- 4 KiB sectors;
- 64 KiB blocks;
- read `0x03`, page program `0x02`, sector erase `0x20`, block erase `0xD8`,
  chip erase `0xC7`, write enable `0x06`, and read status `0x05`;
- busy mask `0x01`;
- ESP-IDF-derived operation timeouts in microseconds;
- no capacity override.

A hand-written default is required. An all-zero derived default would be
invalid.

### Validation and capability

Validation rejects invalid data, such as zero geometry, a zero capacity
override, or a capacity that is not a sector multiple.

Capability is checked per operation, not for the whole description. A chip
description may be useful even if one unused command cannot run on a given
target.

Automatic JEDEC detection remains private implementation detail. Users select
an explicit description when detection is not enough.

Timeout overrides apply only to command-capable execution paths. Dedicated ROM
calls own their timing.

Runtime geometry does not fit directly into `NorFlash` associated constants.
The future external driver must define whether it supports only one compile-time
geometry through those traits, uses wrapper types, or limits variable geometry
to inherent methods. The internal driver's fixed `WRITE_SIZE = 4` and
`ERASE_SIZE = 4096` do not settle this external question. An internal override
must never change those constants; callers with different geometry would use
inherent methods.

### Activation condition

Add this type only when the external driver or a real internal chip override
needs it.

## 3. Internal custom commands (if chip overrides ever return)

The current internal driver uses dedicated ROM functions only. This section
applies only if a demonstrated boot-flash need brings custom commands back.

### Operation dispatch

Each configured operation would choose one of three paths:

| Operation shape | Execution |
|-----------------|-----------|
| Standard command, including capacity-only override | dedicated `esp_rom_spiflash_*` function on every supported chip |
| Command with a one-byte response, such as status | `esp_rom_spiflash_read_user_cmd` on every supported chip |
| Any other custom command, including three-byte JEDEC ID, command-only, address, dummy, write-data, or custom geometry shapes | `spi_flash_hal_common_command` on S3/C2/C3/C5/C6/C61/H2; `NotSupported` on ESP32/S2/P4 |

Capability errors occur when an operation is used. The driver does not reject
an entire configuration because an unused operation is unsupported.

A private transaction description, formerly called `FlashCommand`, can lower
the configured operation into the selected ROM path. There is still no direct
SPI1 register fallback.

### `common_command` and the host context

`spi_flash_hal_common_command` accepts a full `spi_flash_trans_t`. This shape
is defined in IDF's `components/hal/spi_flash_hal_common.inc`. It supports:

- 8-bit or 16-bit commands;
- address and address length;
- dummy cycles;
- write data;
- read data.

The symbol exists on S3/C2/C3/C5/C6/C61/H2 in the `esp-rom-sys/ld/` files. It
is absent on ESP32/S2/P4.

There is a second, older ROM entry point that the target set above misses. The
linker scripts also provide `SPI_Common_Command`, aliased as
`esp_rom_spiflash_common_cmd` in each `*.rom.api.ld`, on **nine** targets:
S2 (`esp32s2.rom.ld:605`), S3 (`:210`), C2, C3, C5, C61, C6, H2 and P4
(`esp32p4.rom.ld:148`). Only ESP32 lacks it, and even there the function is
present in the ROM at `0x4006246c` on both rev0 and rev300; it is simply not
exported by `esp-rom-sys/ld/esp32/`.

So if custom commands ever return, `esp_rom_spiflash_common_cmd` covers a wider
target set than `spi_flash_hal_common_command`, including the S2 and P4 gaps.
Its argument shape differs from `spi_flash_trans_t` and has not been
reverse-engineered here. Check both symbols and both signatures before adding
bindings.

The ROM's `esp_flash_default_chip` cannot be assumed to be initialized in an
esp-hal application:

1. The second-stage bootloader uses the legacy API in
   `bootloader_flash.c:292`, `:396-398`, `:404`, and `:422-425`.
2. IDF application startup calls `esp_flash_init_default_chip()` from
   `components/esp_system/startup.c:341-342`.
3. `esp_flash_spi_init.c` fills the app-RAM `esp_flash_default_host` and
   `default_chip` structures. This still happens under `SPI_FLASH_ROM_IMPL`;
   `flash_ops.c:124` installs only OS hooks.
4. Neither the `esp-rom-sys` linker files nor IDF's ROM linker files export
   `esp_flash_init_default_chip`.

A driver using `common_command` must therefore build and own its host context.
The required data comes from `ESP_FLASH_HOST_CONFIG_DEFAULT` and
`memspi_host_init_pointers`. It should be stored by value and initialized on
first use.

Required `esp-rom-sys` work:

- bind `spi_flash_hal_common_command` on the supported targets;
- define the host structures and initialization data;
- do not bind or depend on `esp_flash_default_chip`.

Hardware validation must cover the distinct ROM and host implementations used
by the supported targets. The implementation must choose representative chips
from source differences, not an arbitrary board count.

### Activation condition

Start this work only if chip configuration returns for a demonstrated internal
boot-flash need.

## 4. Future: a behavioral chip trait

Most chip differences are data and belong in `ChipConfig`. A trait becomes
useful only when a chip needs behavior that fields cannot describe.

Possible examples are:

- vendor-specific Quad Enable procedures;
- 32-bit address mode;
- erase suspend and resume;
- custom protection or one-time-programmable sequences.

The trait should be designed from the first real chip requirement. No method
set is reserved in advance.

### Activation condition

Add a trait only after a supported chip needs behavior that `ChipConfig` cannot
represent.

## 5. Public flash mapping API (`mmap`/`MappedFlash`)

The internal driver keeps MMU code private for encrypted reads. It does not
offer public mapping. The current decision is recorded in
[design A16](FLASH-DESIGN.md#a16-public-mmap-cut-mmu-machinery-stays-private).

Two different public APIs may become useful.

### Lookup an existing mapping

Given a physical flash offset, return the current virtual address or a borrowed
slice if the bootloader already mapped that region.

This needs only an MMU table walk. It creates no entries and owns no pages.
The mapping method is inherent on `Flash`, and the returned view borrows the
driver so write and erase are excluded while the view exists.

### Create a mapping

Map an arbitrary flash region, such as an asset partition, into free MMU pages.
This needs a complete ownership model:

- allocation and accounting of MMU entries;
- coexistence with PSRAM initialization;
- coexistence with temporary encrypted-read mappings;
- unmap on drop;
- cache invalidation rules.

That ownership model may belong in a shared MMU allocator rather than in
`Flash`. Any returned mapping view must borrow the driver so write and erase
remain excluded while the view exists.

### Activation condition

Design a public API only for a real zero-copy consumer and after MMU page
ownership has a clear home.

## 6. Encrypted sector overwrite

The committed `write_encrypted()` API matches ESP-IDF. It writes an aligned range
into previously erased flash and never erases implicitly.

The contract is a uniform 16 bytes
([design A19](FLASH-DESIGN.md#a19-encrypted-writes-match-esp-idf)), so on ESP32 a
row edge decrypts and re-encrypts the adjacent 16-byte block. That is bounded,
row-local, and never erases. The helper below is a different thing: a whole-sector
erase and rewrite. Do not let the former be used to argue the latter is already
half-built.

A future helper may provide overwrite semantics for arbitrary bytes:

```text
read decrypted sector -> merge bytes -> erase sector -> rewrite encrypted
```

That helper needs 4096 bytes of internal-RAM scratch or caller-provided
workspace. Its name, scratch ownership, cancellation behavior, and power-loss
semantics remain undesigned.

### Activation condition

Add this helper only for a real user that cannot follow erase-before-write
semantics.

## 7. Dedicated whole-chip erase

The current driver does not expose a dedicated whole-chip erase operation. The
ROM primitive is deliberately difficult to use safely:

- it is one unchunked call;
- local interrupts stay disabled for the whole operation;
- the other core stays parked under `AutoPark`;
- no watchdog or progress callback can run inside the call;
- it erases the running program's own code.

It is coherent mainly for a flasher stub whose code, data, and stack are fully
resident in internal RAM. A future API must make that narrow execution model
clear instead of presenting whole-chip erase as an ordinary driver operation.
If an inherent `erase_chip()` method is selected, it must remain unstable,
carry `#[ram]`, and document that watchdog or progress servicing is not
possible during the ROM call.

### ROM evidence and ESP32-S2 gap

The dedicated `esp_rom_spiflash_erase_chip` symbol exists on every target
except ESP32-S2. ESP32-S2 instead exports `SPIEraseChip = 0x400170ec`
(`esp32s2.rom.ld:612`) and omits the legacy alias. IDF release/v5.2 at
`72d06017df` shows:

1. `esp32s2.rom.spiflash_legacy.ld` aliases the other legacy operations.
2. IDF does not call the legacy chip-erase name.
3. The compiled ROM patch defines `esp_rom_spiflash_erase_chip` only for
   ESP32.

If this API is implemented, `esp-rom-sys` can add
`esp_rom_spiflash_erase_chip = SPIEraseChip` for ESP32-S2. A destructive
hardware check must still establish whether `SPIEraseChip` waits for
completion. If it returns early, the wrapper must also call the S2
`esp_rom_spiflash_wait_idle` alias.

### Activation condition

Design and implement this only for a concrete RAM-resident flasher or recovery
consumer that cannot use chunked range erase.

## 8. Async internal flash access

The current design keeps the `Dm: DriverMode` type parameter but constructs
only `Flash<'_, Blocking>`. It has no `into_async()` method and no async trait
implementations.

The ROM operations are blocking. A future async wrapper could yield only
between chunks, while the cache is active. Cancellation would leave all
completed chunks committed and later chunks untouched, so a partly completed
erase range would be valid but incomplete. Those semantics need to be useful
to a real consumer before an async surface is added.

### Activation condition

Add an async state only when a consumer benefits from between-chunk yielding
and accepts the resulting cancellation contract.

## 9. Configurable bounce storage

The current driver uses one private static bounce buffer in internal RAM, sized
256 bytes to start with. A later design may make its size or storage configurable
to trade internal RAM for fewer ROM calls; an `esp-config` option is the intended
mechanism, since it keeps the size out of the public API.

The buffer size bounds only the **staged** path. Buffers that are already in
internal RAM and aligned go straight to ROM in 4096-byte chunks, so growing the
buffer changes nothing for them
([design](FLASH-DESIGN.md#chunk-size)). That decoupling is deliberate: an
earlier revision tied both paths to the buffer, which meant this section could
not be revisited without changing the chunking of buffers that are never staged.

Possible forms include a fixed set of supported sizes, caller-provided
internal-RAM workspace, or a configuration option that selects statically
allocated storage. Any design must preserve exclusive access, guarantee
internal-RAM residency, and keep plain slices usable without exposing buffer
placement as part of the stable read or write contract.

### Activation condition

Add configurability only after measurements show that staged transfers are a
bottleneck for a real consumer. The PR A throughput comparison is the first
place that would show up.

## 10. Relax the guard via flash auto-suspend

The committed driver suspends the cache and parks the other core around every ROM
operation, reads included, because Espressif documents that as required
([design B7](FLASH-DESIGN.md#b7-espressifs-documented-spi1-concurrency-constraint)).
That guard is the per-chunk cost and it is what makes dual-core reads disruptive.

There is one sanctioned way to drop it. ESP-IDF's `CONFIG_SPI_FLASH_AUTO_SUSPEND`
lets the cache read flash concurrently with SPI1 operations, and its documentation
is explicit that under that option "the hardware will handle the arbitration
between them". It needs both `SOC_SPI_MEM_SUPPORT_AUTO_SUSPEND` on the chip and a
flash part that supports program/erase suspend, which is why ESP-IDF leaves it off
by default.

A driver-side version would need:

- a chip-capability check, from metadata rather than a `cfg` guess;
- runtime detection that the attached flash part supports suspend, which is
  vendor-specific and is exactly the kind of chip knowledge
  [design A13](FLASH-DESIGN.md#a13-no-chip-configuration-at-launch) currently
  keeps out of the driver;
- a decision about what the guard degrades to, since the flash lock and the
  interrupt disable may still be wanted even when the cache can stay live;
- its own dual-core test, because the failure mode is silent corruption rather
  than a hang.

`CONFIG_SPIRAM_XIP_FROM_PSRAM` is the same shape of escape on chips with
`SOC_SPIRAM_XIP_SUPPORTED`: if code and rodata execute from PSRAM, nothing is
fetching from flash and the guard is unnecessary. That one needs no flash chip
knowledge, only a way to know the application is configured that way.

### Activation condition

Design this only if the PR A interrupt-latency numbers show the guard is a real
problem for a real consumer, and only after the chip and flash-part capability
questions have a home in metadata. Turning down the read chunk limit is the
cheaper first response.
