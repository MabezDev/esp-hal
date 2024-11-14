//! System Timer Test

// esp32 disabled as it does not have a systimer
//% CHIPS: esp32c2 esp32c3 esp32c6 esp32h2 esp32s2 esp32s3

#![no_std]
#![no_main]

use core::cell::RefCell;

use critical_section::Mutex;
use embedded_hal::delay::DelayNs;
use esp_hal::{
    delay::Delay,
    prelude::*,
    timer::{
        systimer::{Alarm, AnyComparator, AnyUnit, FrozenUnit, SystemTimer},
        OneShotTimer,
        PeriodicTimer,
    },
};
use hil_test as _;
use portable_atomic::{AtomicUsize, Ordering};
use static_cell::StaticCell;

static ALARM_TARGET: Mutex<RefCell<Option<OneShotTimer<'static>>>> = Mutex::new(RefCell::new(None));
static ALARM_PERIODIC: Mutex<RefCell<Option<PeriodicTimer<'static>>>> =
    Mutex::new(RefCell::new(None));

struct Context {
    unit: FrozenUnit<'static, AnyUnit<'static>>,
    comparator0: AnyComparator<'static>,
    comparator1: AnyComparator<'static>,
}

#[handler(priority = esp_hal::interrupt::Priority::min())]
fn pass_test_if_called() {
    critical_section::with(|cs| {
        ALARM_TARGET
            .borrow_ref_mut(cs)
            .as_mut()
            .unwrap()
            .clear_interrupt()
    });
    embedded_test::export::check_outcome(());
}

#[handler(priority = esp_hal::interrupt::Priority::min())]
fn handle_periodic_interrupt() {
    critical_section::with(|cs| {
        ALARM_PERIODIC
            .borrow_ref_mut(cs)
            .as_mut()
            .unwrap()
            .clear_interrupt()
    });
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

#[handler(priority = esp_hal::interrupt::Priority::min())]
fn pass_test_if_called_twice() {
    critical_section::with(|cs| {
        ALARM_PERIODIC
            .borrow_ref_mut(cs)
            .as_mut()
            .unwrap()
            .clear_interrupt()
    });
    COUNTER.fetch_add(1, Ordering::Relaxed);
    if COUNTER.load(Ordering::Relaxed) == 2 {
        embedded_test::export::check_outcome(());
    }
}

#[handler(priority = esp_hal::interrupt::Priority::min())]
fn target_fail_test_if_called_twice() {
    critical_section::with(|cs| {
        ALARM_TARGET
            .borrow_ref_mut(cs)
            .as_mut()
            .unwrap()
            .clear_interrupt()
    });
    COUNTER.fetch_add(1, Ordering::Relaxed);
    assert!(COUNTER.load(Ordering::Relaxed) != 2);
}

#[cfg(test)]
#[embedded_test::tests]
mod tests {
    use super::*;

    #[init]
    fn init() -> Context {
        let peripherals = esp_hal::init(esp_hal::Config::default());

        let systimer = SystemTimer::new(peripherals.SYSTIMER);
        static UNIT0: StaticCell<AnyUnit<'static>> = StaticCell::new();

        let unit0 = UNIT0.init(systimer.unit0.into());
        let frozen_unit = FrozenUnit::new(unit0);

        Context {
            unit: frozen_unit,
            comparator0: systimer.comparator0.into(),
            comparator1: systimer.comparator1.into(),
        }
    }

    #[test]
    #[timeout(3)]
    fn target_interrupt_is_handled(ctx: Context) {
        let mut alarm0 = OneShotTimer::new(Alarm::new(ctx.comparator0, &ctx.unit));

        critical_section::with(|cs| {
            alarm0.set_interrupt_handler(pass_test_if_called);
            alarm0.schedule(10u64.millis()).unwrap();
            alarm0.enable_interrupt(true);

            ALARM_TARGET.borrow_ref_mut(cs).replace(alarm0);
        });

        // We'll end the test in the interrupt handler.
        loop {}
    }

    #[test]
    #[timeout(3)]
    fn target_interrupt_is_handled_once(ctx: Context) {
        let mut alarm0 = OneShotTimer::new(Alarm::new(ctx.comparator0, &ctx.unit));
        let mut alarm1 = PeriodicTimer::new(Alarm::new(ctx.comparator1, &ctx.unit));

        COUNTER.store(0, Ordering::Relaxed);

        critical_section::with(|cs| {
            alarm0.set_interrupt_handler(target_fail_test_if_called_twice);
            alarm0.schedule(10u64.millis()).unwrap();
            alarm0.enable_interrupt(true);

            alarm1.set_interrupt_handler(handle_periodic_interrupt);
            alarm1.start(100u64.millis()).unwrap();
            alarm1.enable_interrupt(true);

            ALARM_TARGET.borrow_ref_mut(cs).replace(alarm0);
            ALARM_PERIODIC.borrow_ref_mut(cs).replace(alarm1);
        });

        let mut delay = Delay::new();
        delay.delay_ms(300);
    }

    #[test]
    #[timeout(3)]
    fn periodic_interrupt_is_handled(ctx: Context) {
        let mut alarm1 = PeriodicTimer::new(Alarm::new(ctx.comparator1, &ctx.unit));

        COUNTER.store(0, Ordering::Relaxed);

        critical_section::with(|cs| {
            alarm1.set_interrupt_handler(pass_test_if_called_twice);
            alarm1.start(100u64.millis()).unwrap();
            alarm1.enable_interrupt(true);

            ALARM_PERIODIC.borrow_ref_mut(cs).replace(alarm1);
        });

        // We'll end the test in the interrupt handler.
        loop {}
    }
}
