//! # Wireless support for Espressif ESP32 devices.
//!
//! This documentation is built for the configured chip.
//! Please ensure you are reading the correct documentation for your target
//! device.
//!
//! ## Usage
//!
//! ### Importing
//!
//! Note that this module currently requires you to enable the `unstable` feature
//! on `esp-hal`.
//!
//! Ensure that the right features are enabled for your chip. See [Examples](https://github.com/esp-rs/esp-hal/tree/main/examples#examples) for more examples.
//!
//! You will also need a dynamic memory allocator, and a preemptive task scheduler in your
//! application. For the dynamic allocator, we recommend using `esp-alloc`. For the task scheduler,
//! the simplest option that is supported by us is `esp-rtos`, but you may use Ariel
//! OS or other operating systems as well.
//!
//! ```toml
//! [dependencies.esp-hal]
//! # A supported chip needs to be specified, as well as specific use-case features
//! features = ["esp32c6", "wifi", "esp-now", "radio-esp-alloc"]
//! [dependencies.esp-rtos]
//! features = ["esp32c6", "radio", "esp-alloc"]
//! [dependencies.esp-alloc]
//! features = []
//! ```
//!
//! ### Optimization Level
//!
//! It is necessary to build with optimization level 2 or 3 since otherwise, it
//! might not even be able to connect or advertise.
//!
//! To make it work also for your debug builds add this to your `Cargo.toml`
//! ```toml
//! [profile.dev.package.esp-hal]
//! opt-level = 3
//! ```
//! ## Globally disable logging
//!
//! The radio module contains a lot of trace-level logging statements.
//! For maximum performance you might want to disable logging via
//! a feature flag of the `log` crate. See [documentation](https://docs.rs/log/0.4.19/log/#compile-time-filters).
//! You should set it to `release_max_level_off`.
//!
//! ### Wi-Fi performance considerations
//!
//! The default configuration is quite conservative to reduce power and memory consumption.
//!
//! There are a number of settings which influence the general performance. Optimal settings are chip and applications specific.
//! You can get inspiration from the [ESP-IDF examples](https://github.com/espressif/esp-idf/tree/release/v5.3/examples/wifi/iperf)
//!
//! Please note that the configuration keys are usually named slightly different and not all configuration keys apply.
#![cfg_attr(
    feature = "wifi",
    doc = "By default the power-saving mode is [`PowerSaveMode::None`](crate::radio::wifi::PowerSaveMode::None) and `ESP_PHY_CONFIG_PHY_ENABLE_USB` is enabled by default."
)]
//! In addition pay attention to these configuration keys:
//! - `ESP_HAL_CONFIG_RADIO_RX_QUEUE_SIZE`
//! - `ESP_HAL_CONFIG_RADIO_TX_QUEUE_SIZE`
//! - `ESP_HAL_CONFIG_RADIO_MAX_BURST_SIZE`
#![cfg_attr(
    multi_core,
    doc = concat!(
        "### Running on the Second Core",
        "\n\n",
        "BLE and Wi-Fi can also be run on the second core.",
        "\n\n",
        "`esp_hal::radio::init` is recommended to be called on the first core. The tasks ",
        "created are pinned to the first core.",
        "\n\n",
        "It's also important to allocate adequate stack for the second core; in many ",
        "cases 8kB is not enough, and 16kB or more may be required depending on your ",
        "use case. Failing to allocate adequate stack may result in strange behaviour, ",
        "such as your application silently failing at some point during execution."
    )
)]
//! ## Feature flags
//!
//! Note that not all features are available on every MCU. For example, `ble`
//! (and thus, `coex`) is not available on ESP32-S2.
//!
//! When using the `dump_packets` config you can use the extcap in
//! `extras/esp-wifishark` to analyze the frames in Wireshark.
//! For more information see
//! [extras/esp-wifishark/README.md](../extras/esp-wifishark/README.md)
//!
//! ## Additional configuration
//!
//! We've exposed some configuration options that don't fit into cargo
//! features. These can be set via environment variables, or via cargo's `[env]`
//! section inside `.cargo/config.toml`. See the esp-hal crate documentation
//! for the full list of configuration options.

// MUST be the first module
mod fmt;

#[cfg(any(feature = "wifi", feature = "ble"))]
use crate as esp_hal_crate;
#[cfg(feature = "unstable")]
#[cfg_attr(docsrs, doc(cfg(feature = "unstable")))]
pub use phy::CalibrationResult;
#[cfg(not(feature = "unstable"))]
use phy::CalibrationResult;
use esp_radio_rtos_driver as preempt;
use esp_sync::RawMutex;
#[cfg(any(feature = "wifi", feature = "ble"))]
use docsplay::Display;
#[cfg(esp32)]
use esp_hal_crate::analog::adc::{release_adc2, try_claim_adc2};
#[cfg(any(feature = "wifi", feature = "ble"))]
use esp_hal_crate::{
    clock::{Clocks, init_radio_clocks},
    time::Rate,
};
use sys::include::esp_phy_calibration_data_t;

#[cfg(feature = "ble")]
pub use private::InitializationError;
#[cfg(all(not(feature = "ble"), feature = "wifi"))]
use private::InitializationError;
pub(crate) mod sys {
    #[cfg(esp32)]
    pub use esp_wifi_sys_esp32::*;
    #[cfg(esp32c2)]
    pub use esp_wifi_sys_esp32c2::*;
    #[cfg(esp32c3)]
    pub use esp_wifi_sys_esp32c3::*;
    #[cfg(esp32c6)]
    pub use esp_wifi_sys_esp32c6::*;
    #[cfg(esp32h2)]
    pub use esp_wifi_sys_esp32h2::*;
    #[cfg(esp32s2)]
    pub use esp_wifi_sys_esp32s2::*;
    #[cfg(esp32s3)]
    pub use esp_wifi_sys_esp32s3::*;
}

#[cfg(any(feature = "wifi", feature = "ble"))]
use radio_hal::{setup_radio_isr, shutdown_radio_isr};
#[cfg(feature = "wifi")]
use wifi::WifiError;

// can't use instability on inline module definitions, see https://github.com/rust-lang/rust/issues/54727
#[doc(hidden)]
macro_rules! unstable_module {
    ($(
        $(#[$meta:meta])*
        pub mod $module:ident;
    )*) => {
        $(
            $(#[$meta])*
            #[cfg(feature = "unstable")]
            #[cfg_attr(docsrs, doc(cfg(feature = "unstable")))]
            pub mod $module;

            $(#[$meta])*
            #[cfg(not(feature = "unstable"))]
            #[cfg_attr(docsrs, doc(cfg(feature = "unstable")))]
            #[allow(unused)]
            pub(crate) mod $module;
        )*
    };
}

mod compat;

mod radio_hal;
mod phy;
mod time;

#[cfg(feature = "wifi")]
pub mod wifi;

unstable_module! {
    #[cfg(feature = "esp-now")]
    #[cfg_attr(docsrs, doc(cfg(feature = "esp-now")))]
    pub mod esp_now;
    #[cfg(feature = "ble")]
    #[cfg_attr(docsrs, doc(cfg(feature = "ble")))]
    pub mod ble;
    #[cfg(feature = "ieee802154")]
    #[cfg_attr(docsrs, doc(cfg(feature = "ieee802154")))]
    pub mod ieee802154;
}

pub(crate) mod common_adapter;
pub(crate) mod memory_fence;

pub(crate) static ESP_RADIO_LOCK: RawMutex = RawMutex::new();

#[cfg(any(feature = "wifi", feature = "ble"))]
static RADIO_REFCOUNT: critical_section::Mutex<core::cell::Cell<u32>> =
    critical_section::Mutex::new(core::cell::Cell::new(0));

// this is just to verify that we use the correct defaults in `build.rs`
#[allow(clippy::assertions_on_constants)] // TODO: try assert_eq once it's usable in const context
const _: () = {
    cfg_if::cfg_if! {
        if #[cfg(not(esp32h2))] {
            core::assert!(sys::include::CONFIG_ESP_WIFI_STATIC_RX_BUFFER_NUM == 10);
            core::assert!(sys::include::CONFIG_ESP_WIFI_DYNAMIC_RX_BUFFER_NUM == 32);
            core::assert!(sys::include::WIFI_STATIC_TX_BUFFER_NUM == 0);
            core::assert!(sys::include::CONFIG_ESP_WIFI_DYNAMIC_RX_BUFFER_NUM == 32);
            core::assert!(sys::include::CONFIG_ESP_WIFI_AMPDU_RX_ENABLED == 1);
            core::assert!(sys::include::CONFIG_ESP_WIFI_AMPDU_TX_ENABLED == 1);
            core::assert!(sys::include::WIFI_AMSDU_TX_ENABLED == 0);
            core::assert!(sys::include::CONFIG_ESP32_WIFI_RX_BA_WIN == 6);
        }
    };
};

#[procmacros::doc_replace]
/// Initialize for using Wi-Fi and or BLE.
///
/// Wi-Fi and BLE require a preemptive scheduler to be present. Without one, the underlying firmware
/// can't operate. The scheduler must implement the interfaces in the `esp-radio-rtos-driver`
/// crate. If you are using an embedded RTOS like Ariel OS, it needs to provide an appropriate
/// implementation.
///
/// If you are not using an embedded RTOS, use the `esp-rtos` crate which provides the
/// necessary functionality.
///
/// Make sure to **not** call this function while interrupts are disabled.
///
/// ## Errors
///
/// - The function may return an error if the scheduler is not initialized.
#[cfg_attr(
    esp32,
    doc = " - The function may return an error if ADC2 is already in use."
)]
/// - The function may return an error if interrupts are disabled.
/// - The function may return an error if initializing the underlying driver fails.
#[cfg(any(feature = "wifi", feature = "ble"))]
pub(crate) fn init() -> Result<(), InitializationError> {
    #[cfg(esp32)]
    if try_claim_adc2(unsafe { crate::Internal::conjure() }).is_err() {
        return Err(InitializationError::Adc2IsUsed);
    }

    if !preempt::initialized() {
        return Err(InitializationError::SchedulerNotInitialized);
    }

    // A minimum clock of 80MHz is required to operate Wi-Fi module.
    const MIN_CLOCK: Rate = Rate::from_mhz(80);
    let clocks = Clocks::get();
    if clocks.cpu_clock < MIN_CLOCK {
        return Err(InitializationError::WrongClockConfig);
    }

    common_adapter::enable_wifi_power_domain();

    setup_radio_isr();

    wifi_set_log_verbose();
    init_radio_clocks();

    #[cfg(coex)]
    match wifi::coex_initialize() {
        0 => {}
        error => panic!("Failed to initialize coexistence, error code: {}", error),
    }

    debug!("Radio initialized");

    Ok(())
}

#[cfg(any(feature = "wifi", feature = "ble"))]
pub(crate) fn deinit() {
    // Disable coexistence
    #[cfg(coex)]
    {
        unsafe { wifi::os_adapter::coex_disable() };
        unsafe { wifi::os_adapter::coex_deinit() };
    }

    shutdown_radio_isr();

    #[cfg(esp32)]
    // Allow using `ADC2` again
    release_adc2(unsafe { crate::Internal::conjure() });

    debug!("Radio deinitialized");
}

/// Management of the global reference count
/// and conditional hardware initialization/deinitialization.
#[cfg(any(feature = "wifi", feature = "ble"))]
#[derive(Debug)]
pub(crate) struct RadioRefGuard;

#[cfg(any(feature = "wifi", feature = "ble"))]
impl RadioRefGuard {
    /// Increments the refcount. If the old count was 0, it performs hardware init.
    /// If hardware init fails, it rolls back the refcount only once.
    fn new() -> Result<Self, InitializationError> {
        critical_section::with(|cs| {
            debug!("Creating RadioRefGuard");
            let rc = RADIO_REFCOUNT.borrow(cs);

            let prev = rc.get();
            rc.set(prev + 1);

            if prev == 0
                && let Err(e) = init()
            {
                rc.set(prev);
                return Err(e);
            }

            Ok(RadioRefGuard)
        })
    }
}

#[cfg(any(feature = "wifi", feature = "ble"))]
impl Drop for RadioRefGuard {
    /// Decrements the refcount. If the count drops to 0, it performs hardware de-init.
    fn drop(&mut self) {
        critical_section::with(|cs| {
            debug!("Dropping RadioRefGuard");
            let rc = RADIO_REFCOUNT.borrow(cs);

            let prev = rc.get();
            rc.set(prev - 1);

            if prev == 1 {
                // Last user dropped, run de-initialization
                deinit();
            }
        });
    }
}

/// Returns true if at least some interrupt levels are disabled.
#[cfg(any(feature = "wifi", all(feature = "ble", bt_controller = "btdm")))]
fn is_interrupts_disabled() -> bool {
    #[cfg(target_arch = "xtensa")]
    return crate::xtensa_lx::interrupt::get_level() != 0
        || crate::xtensa_lx::interrupt::get_mask() == 0;

    #[cfg(target_arch = "riscv32")]
    return !crate::riscv::register::mstatus::read().mie()
        || crate::interrupt::current_runlevel() >= crate::interrupt::Priority::Priority1;
}

/// Enable verbose logging within the Wi-Fi driver
/// Does nothing unless the `print-logs-from-driver` feature is enabled.
#[instability::unstable]
pub fn wifi_set_log_verbose() {
    #[cfg(all(feature = "print-logs-from-driver", not(esp32h2)))]
    unsafe {
        use sys::include::{
            esp_wifi_internal_set_log_level,
            wifi_log_level_t_WIFI_LOG_VERBOSE,
        };

        esp_wifi_internal_set_log_level(wifi_log_level_t_WIFI_LOG_VERBOSE);
    }
}

/// Get calibration data.
///
/// Returns the last calibration result.
///
/// If [last_calibration_result] returns [CalibrationResult::DataCheckFailed], consider persisting
/// the new data.
#[instability::unstable]
pub fn phy_calibration_data(data: &mut [u8; phy::PHY_CALIBRATION_DATA_LENGTH]) {
    let _ = phy::backup_phy_calibration_data(data);
}

/// Set calibration data.
///
/// This will be used next time the phy gets initialized.
#[instability::unstable]
pub fn set_phy_calibration_data(data: &[u8; core::mem::size_of::<esp_phy_calibration_data_t>()]) {
    // Although we're ignoring the result here, this doesn't change the behavior, as this just
    // doesn't do anything in case an error is returned.
    let _ = phy::set_phy_calibration_data(data);
}

/// Get the last calibration result.
///
/// This can be used to know if any previously persisted calibration data is outdated/invalid and
/// needs to get updated.
#[instability::unstable]
pub fn last_calibration_result() -> Option<CalibrationResult> {
    phy::last_calibration_result()
}

#[cfg(any(feature = "wifi", feature = "ble"))]
mod private {
    use super::Display;
    #[cfg(feature = "wifi")]
    use super::wifi::WifiError;
    #[derive(Display, Debug, Clone, Copy, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "defmt", derive(defmt::Format))]
    /// Error which can be returned during radio initialization.
    #[non_exhaustive]
    pub enum InitializationError {
        /// An error from the Wi-Fi driver: {0}.
        #[cfg(feature = "wifi")]
        WifiError(WifiError),
        /// The current CPU clock frequency is too low.
        WrongClockConfig,
        /// The scheduler is not initialized.
        SchedulerNotInitialized,
        #[cfg(esp32)]
        /// ADC2 is required by esp-radio, but it is in use by esp-hal.
        Adc2IsUsed,
    }

    impl core::error::Error for InitializationError {}

    #[cfg(feature = "wifi")]
    impl From<WifiError> for InitializationError {
        fn from(value: WifiError) -> Self {
            InitializationError::WifiError(value)
        }
    }
}
