# `esp_hal::flash` — Deferred material (annex)

Extracted from [FLASH-DESIGN.md](FLASH-DESIGN.md) when the external SPI backend was split out of `Flash` (design [A12](FLASH-DESIGN.md#a12--internal-only-scope-the-external-backend-splits-out)) and chip configuration was cut from the internal driver (design [A13](FLASH-DESIGN.md#a13--no-chip-configuration-at-launch)). **Non-normative**: this is a parts shelf, not a plan — nothing here is committed work, and nothing on the fast path depends on it. Re-verify evidence against current ROMs/IDF sources before building on it.

Contents:
1. [External SPI NOR flash driver](#1-external-spi-nor-flash-driver) — the former `Spi` backend, now a future standalone driver
2. [`ChipConfig` — the chip parameter vocabulary](#2-chipconfig--the-chip-parameter-vocabulary)
3. [Internal custom commands](#3-internal-custom-commands-if-chip-overrides-ever-return) — three-step resolution, B3/A11 evidence, the hardware spike
4. [Behavioral chip trait](#4-future-a-behavioral-chip-trait)
5. [Public flash mapping API](#5-public-flash-mapping-api-mmapmappedflash) — the cut `mmap`/`MappedFlash` surface

---

## 1. External SPI NOR flash driver

**What it was**: the `Spi` backend of `Flash`, constructed via `new_spi(Spi<'d, Blocking>, Config)` and erased into a private backend enum so `Flash` carried no extra type parameters.

**Why it split** (full rationale: design [A12](FLASH-DESIGN.md#a12--internal-only-scope-the-external-backend-splits-out)): `mmap` and encrypted access are structurally impossible off the SPI0/SPI1 path; the backends shared almost nothing beyond the operation list; the erasure smeared internal-only and external-only concerns across shared `Config`/`Error`/docs and forced exclusive bus ownership. The future external driver is its own type, unified with `esp_hal::flash::Flash` through the embedded-storage traits.

### Design fragments worth keeping

- **Constructor** (former decision A8): the driver takes a *fully configured* `Spi` — pins including hardware CS, mode, frequency — and consumes it; the flash driver does not route pins or own bus setup. As a standalone type this decision is **reopened**: a generic `SpiDevice` (embedded-hal-bus) constructor becomes possible — the erased design could not carry type parameters, a standalone driver can. Bus sharing (display + flash on one SPI) is a real use case the erasure excluded; the old module-doc limitation "an external flash chip owns its SPI bus" was a *consequence of the erasure*, not a law.
- **Execution model**: every operation lowers to a private `FlashCommand` (command / address / dummy / data phases) executed as a half-duplex master transaction, building on the existing SPI master half-duplex API (`Command`/`Address` phases + DMA + hardware CS) — a pattern already proven in-tree (C7 below). Busy-waits are RDSR polling against the chip's busy-status mask with timeouts, never fixed delays.
- **Async**: start a DMA transfer, await completion via interrupt — genuinely interrupt/DMA-driven, unlike the internal driver's chunk-yield async. `into_async` here really does bind a handler (and inherits the `Async: !Send` contract meaningfully).
- **Error design**: the external driver owns its own error type (DMA failure variants etc.) — no longer constrained to share `flash::Error` (see the superseded note in design [A6](FLASH-DESIGN.md#a6--error-type-shape)).
- **Quad/Dual IO modes**: external-driver territory; the internal boot flash's IO mode is the ROM/bootloader's business.
- **P4 caveat**: the DMA-driven path inherits the P4 SPI-DMA gap until that lands (see the C7 chip filter).

### Open questions for whoever builds it

- Name (`SpiNorFlash`?) and home: esp-hal sibling module vs a separate crate. After the split nothing ESP-specific remains — over embedded-hal traits it would serve any target; over esp-hal's `Spi` it keeps first-class DMA integration. Genuinely open.
- Owned `Spi` vs generic `SpiDevice` (or both constructors).
- Config shape — probably takes the `ChipConfig` vocabulary (§2) directly; there is no multi-core/parking concern and no residency rule, so the internal driver's `Config` does not apply.
- Trait unification story with `esp_hal::flash::Flash` (embedded-storage impls, `READ_SIZE`/`WRITE_SIZE`/`ERASE_SIZE` from `ChipConfig` geometry vs consts).

### C7 — External-backend prior art

`qa-test/src/bin/qspi_flash.rs` already drives a GD25Q64C on SPI2 with raw half-duplex commands (0x06/0x20/0x32/0xEB) and fixed `delay_millis(250)` waits — the external driver is that pattern productized. Its chip filter is `spi_master_supports_dma && !esp32p4`: the DMA-driven async path inherits the P4 SPI-DMA gap until that lands. The file stays as-is in-tree and becomes the external driver's qa test when that driver exists.

---

## 2. `ChipConfig` — the chip parameter vocabulary

The former data-driven chip override: "the config *is* the driver" — there is deliberately no chip-driver *trait*, because it would carry only data and no behavior (genuine behavior is §4's territory). Primary future consumer: the external driver (§1). The `capacity` field's original job — recourse for failed detection on the internal driver — now lives on the internal `Config` as the unstable `with_capacity` override (design [A13](FLASH-DESIGN.md#a13--no-chip-configuration-at-launch)).

```rust
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, BuilderLite)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ChipConfig {
    /// `None` = keep the auto-detected capacity, so command/timing/geometry
    /// overrides stay composable with detection and `default()` is valid to
    /// apply. (BuilderLite generates `with_capacity(u32)` — auto-wrapped in
    /// `Some` — plus `with_capacity_none()`.)
    capacity: Option<u32>,
    sector_size: u32,
    block_size: u32,
    page_size: u32,

    read_command: u8,
    page_program_command: u8,
    sector_erase_command: u8,
    block_erase_command: u8,
    chip_erase_command: u8,
    write_enable_command: u8,
    read_status_command: u8,
    status_busy_mask: u8,

    page_program_timeout_us: u32,
    sector_erase_timeout_us: u32,
    block_erase_timeout_us: u32,
    chip_erase_timeout_us: u32,
}
```

- `Default` is **hand-written**, not derived (a derived default would be all-zero commands and zero geometry — guaranteed nonsense). Default = the standard 25-series command set (read 0x03, page-program 0x02, sector-erase 0x20, block-erase 0xD8, chip-erase 0xC7, write-enable 0x06, read-status 0x05, busy mask 0x01), 256 B page, 4 KiB sector, 64 KiB block, ESP-IDF-derived timeouts, `capacity: None` (keep auto-detected). `ChipConfig::default()` is therefore valid to apply.
- Validation (the former `ConfigError::InvalidChipConfig`): reject only explicitly invalid overrides — zero capacity override or page size, capacity not a multiple of the sector size, ... Validation checks *values*, never chip capability: capability is per-operation, at the point of use.
- Timeout overrides apply only on command-capable execution paths — the plain ROM functions own their timing.
- Geometry overrides must **not** change the embedded-storage `WRITE_SIZE`/`ERASE_SIZE` consts (fixed at the ESP hardware values, 4 / 4096); a chip with non-standard geometry uses the inherent `erase()`/`write()` and not the const-based `NorFlash` trait.
- Auto-detection (JEDEC ID → known config) stays internal — a private table, not a user-implementable hook; the override path is how a user states their chip explicitly.

---

## 3. Internal custom commands (if chip overrides ever return)

The internal backend's former three-step per-operation resolution, designed around user operations rather than ROM entry points (design [A3](FLASH-DESIGN.md#a3--ops-shaped-interface-principle-retained-backend-enum-dissolved) — the totality principle survives in the main design; the multi-primitive resolution below is what moved here):

| Operation, as configured | Internal execution |
|--------------------------|--------------------|
| default commands (incl. capacity-only override) | dedicated `esp_rom_spiflash_*` — all 10 chips (S2 `erase_chip` via the `SPIEraseChip` alias — design [B1](FLASH-DESIGN.md#b1--dedicated-rom-functions-and-the-s2-erase_chip-gap) correction) |
| custom command, command+response shape (status reads, JEDEC ID) | `esp_rom_spiflash_read_user_cmd` — all 10 chips (design [B2](FLASH-DESIGN.md#b2--read_user_cmd-availability-and-shape): no address phase, no dummy cycles, no write-data phase) |
| custom command needing address/dummy/data phases (read, program, erase), or custom geometry the dedicated ROM functions can't honor | `spi_flash_hal_common_command` with a **driver-built host context** — S3/C2/C3/C5/C6/C61/H2 (B3 below); **`Error::NotSupported` on ESP32/S2/P4** |

Capability errors surface **per operation, at the point of use** — never at config time: a config is legitimate if the operations you actually use are executable (example: a custom busy-poll command works on every chip; rejecting the whole config because its never-called custom erase command can't run there would throw that away). There is deliberately no SPI1 register-exec fallback for the `NotSupported` cells (design [A1](FLASH-DESIGN.md#a1--no-spi1-register-exec-fallback)) — no RAM-resident register bit-banging; per-op `NotSupported` → supported later is additive.

`FlashCommand` (the private phase-described transaction type) was the shared lowering between the `common_command` path and the external driver's SPI-master path.

### B3 — common_command and the host context

`spi_flash_hal_common_command` exists on **exactly S3/C2/C3/C5/C6/C61/H2** (verified against `esp-rom-sys/ld/`; zero hits under esp32/esp32s2/esp32p4). It executes a full `spi_flash_trans_t` — 8/16-bit command, address + bit length, dummy cycles, mosi and miso data (verified against IDF `components/hal/spi_flash_hal_common.inc`). Its absence is the precise reason full `ChipConfig` overrides were `NotSupported` on ESP32/S2/P4.

**The ROM's `esp_flash_default_chip` global is never initialized under esp-hal's boot flow** — the evidence chain:

1. The 2nd-stage bootloader definitively does not initialize it: every bootloader flash op goes through the *legacy* `esp_rom_spiflash_*` API (`bootloader_flash.c:292` read, `:396-398` write, `:404`/`:422-425` erase), and no `bootloader_support` source uses the modern `esp_flash_*` driver at all.
2. IDF *app* startup initializes it unconditionally (`components/esp_system/startup.c:341-342` → `esp_flash_init_default_chip()`, a full from-scratch probe in `esp_flash_spi_init.c` that fills **app-RAM** statics — `DRAM_ATTR esp_flash_default_host` / `default_chip` — and reads the ROM-boot-populated legacy `g_rom_flashchip` back as ground truth). This runs **even under `SPI_FLASH_ROM_IMPL`**, where `spi_flash_rom_impl_init()` wires only OS hooks (`flash_ops.c:124`) — Espressif's own code treats the ROM instance as uninitialized at app entry.
3. No init function exists in any ROM export — neither esp-rom-sys's ld scripts nor IDF's full `components/esp_rom/*/ld/` set contains `esp_flash_init_default_chip` or any equivalent.
4. ROM-side, confirmed by maintainer ROM knowledge: ROM startup initializes only the `.data`/`.bss` sections of its reserved memory area and functionally populates only the legacy `g_rom_flashchip` (the `g_rom_` prefix marks ROM-populated data) — the ROM has no concept of the IDF-side esp_flash symbols.

### A11 — Driver-built host context

Consequence of B3: a driver using `common_command` **builds its own host context**. `spi_flash_hal_common_command` takes the host instance as an argument, and the IDF-side init is a plain data fill (`memspi_host_init_pointers` + `ESP_FLASH_HOST_CONFIG_DEFAULT`) replicated in Rust as driver-owned data — the Rust equivalent of IDF's init, owned by value (also what kept `Flash<'_, Blocking>: Send` with no raw ROM pointers cached), lazily initialized on first step-3 use. No `esp_flash_default_chip` binding, no dependence on ROM-version or boot-time state.

**Residual hardware spike** (blocks only this section's work): validate the self-built host context against ROM `spi_flash_hal_common_command` on hardware, 2–3 chips.

### `esp-rom-sys` work this would need

Bind `spi_flash_hal_common_command` on the 7 capable chips (no `esp_flash_default_chip` binding — see A11 above).

---

## 4. Future: a behavioral chip trait

A trait becomes justified only once a chip needs genuine *behavior* that `ChipConfig` can't encode as data — quad-enable procedures, 32-bit addressing for >16 MB parts, erase suspend/resume, custom protection/OTP sequences (cf. ESP-IDF's `spi_flash_chip_t` vtable). All future and unstable: introduce the trait then, with the signatures the real need reveals — not speculatively now.

---

## 5. Public flash mapping API (`mmap`/`MappedFlash`)

Cut from the committed surface (design [A16](FLASH-DESIGN.md#a16--public-mmap-cut-mmu-machinery-stays-private)): the bootloader establishes the running app's flash mappings before esp-hal executes, so the driver neither creates nor owns them. The MMU machinery itself ships regardless, as private internals (`mmu.rs`) for `read_encrypted`; what is deferred is any *public* mapping surface. Two candidate shapes, in ascending order of work:

- **Lookup over existing mappings** (IDF `spi_flash_phys2cache` / `spi_flash_cache2phys` analogue): given a flash offset, return the virtual address — or a borrowed `&[u8]` view — if the region is currently cache-mapped (the app image's segments are, courtesy of the bootloader). Cheap: an MMU table walk, no allocation, no new mappings. Borrowing the driver gives the same no-write-while-viewing discipline `MappedFlash` had. No known consumer yet; that is the only reason it sits here and not in the design.
- **Mapping creation** (IDF `spi_flash_mmap` / `esp_partition_mmap` analogue): map an arbitrary flash region — e.g. an asset partition, which is *not* pre-mapped by the bootloader — into free MMU pages for zero-copy reads. This is the shape the original design called `mmap`, and it requires what esp-hal lacks: an MMU-page ownership story (free-slot allocation and accounting, coexistence with PSRAM init — which also programs MMU entries — and with the transient encrypted-read mappings, unmap-on-drop, cache invalidation policy). Design it against a real consumer, on top of whatever MMU allocator esp-hal grows.

Semantics worth carrying into either revival: the original `MappedFlash` borrowed the driver, so write/erase were compile-time excluded while a view was alive.

---

## Moved glossary entries

- **ROM HAL / `common_command`** — the modern IDF flash HAL compiled into some ROMs; `spi_flash_hal_common_command` executes one fully-described transaction (command/address/dummy/data).
- **Host context** — the `spi_flash_host_inst_t` data structure the ROM HAL operates on. Under esp-hal it would be driver-built (B3/A11 above); the ROM's own `esp_flash_default_chip` instance is never initialized outside IDF apps.
