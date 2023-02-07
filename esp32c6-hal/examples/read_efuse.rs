//! This shows how to read selected information from eFuses.
//! e.g. the MAC address

#![no_std]
#![no_main]

use esp32c6_hal::{
    clock::ClockControl,
    efuse::Efuse,
    peripherals::Peripherals,
    prelude::*,
    timer::TimerGroup,
    Rtc,
};
use esp_backtrace as _;
use esp_println::println;
use esp_riscv_rt::entry;

#[entry]
fn main() -> ! {
    let peripherals = Peripherals::take();
    let system = peripherals.PCR.split();
    let clocks = ClockControl::boot_defaults(system.clock_control).freeze();

    let mut rtc = Rtc::new(peripherals.LP_CLKRST);
    let timer_group0 = TimerGroup::new(peripherals.TIMG0, &clocks);
    let _wdt0 = timer_group0.wdt;
    let timer_group1 = TimerGroup::new(peripherals.TIMG1, &clocks);
    let _wdt1 = timer_group1.wdt;

    // Disable watchdog timers
    rtc.swd.disable();
    rtc.rwdt.disable();

    println!("MAC address {:02x?}", Efuse::get_mac_address());
    println!("Flash Encryption {:?}", Efuse::get_flash_encryption());

    loop {}
}