//! Blinks an LED
//!
//! The following wiring is assumed:
//! - LED => GPIO0

//% CHIPS: esp32 esp32c2 esp32c3 esp32c6 esp32h2 esp32s2 esp32s3

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::{
    delay::Delay,
    gpio::{Io, Level, Output},
    prelude::*,
};

use core::sync::atomic::{AtomicU8, Ordering};

#[allow(clippy::declare_interior_mutable_const)]
const EMPTY_CELL: AtomicU8 = AtomicU8::new(0);

const N: usize = 8;
const MASK: u8 = N as u8 - 1;

pub struct MpMcQueue {
    buffer: [AtomicU8; N],
    dequeue_pos: AtomicU8,
    enqueue_pos: AtomicU8,
}

impl MpMcQueue {
    pub const fn new() -> Self {
        let mut cell_count = 0;
        let mut result_cells = [EMPTY_CELL; N];
        while cell_count != N {
            result_cells[cell_count] = AtomicU8::new(cell_count as u8);
            cell_count += 1;
        }

        Self {
            buffer: result_cells,
            dequeue_pos: AtomicU8::new(0),
            enqueue_pos: AtomicU8::new(0),
        }
    }

    pub fn dequeue(&self) -> bool {
        let mut pos = self.dequeue_pos.load(Ordering::Relaxed);
        let mut cell;
        loop {
            cell = &self.buffer[(pos & MASK) as usize];
            let seq = cell.load(Ordering::Acquire);
            match (seq as i8).wrapping_sub((pos.wrapping_add(1)) as i8) {
                0 => {
                    if self
                        .dequeue_pos
                        .compare_exchange_weak(
                            pos,
                            pos.wrapping_add(1),
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        )
                        .is_ok()
                    {
                        break;
                    }
                }
                i8::MIN..=-1 => return false,
                _ => {
                    pos = self.dequeue_pos.load(Ordering::Relaxed);
                }
            }
        }
        cell.store(pos.wrapping_add(MASK).wrapping_add(1), Ordering::Release);
        true
    }

    pub fn enqueue(&self) -> bool {
        let mut pos = self.enqueue_pos.load(Ordering::Relaxed);
        let mut cell;
        loop {
            cell = &self.buffer[(pos & MASK) as usize];
            let seq = cell.load(Ordering::Acquire);
            match (seq as i8).wrapping_sub(pos as i8) {
                0 => {
                    if self
                        .enqueue_pos
                        .compare_exchange_weak(
                            pos,
                            pos.wrapping_add(1),
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        )
                        .is_ok()
                    {
                        break;
                    }
                }
                i8::MIN..=-1 => return false,
                _ => {
                    pos = self.enqueue_pos.load(Ordering::Relaxed);
                }
            }
        }
        cell.store(pos.wrapping_add(1), Ordering::Release);
        true
    }
}

#[inline(never)]
fn inner() {
    let queue = MpMcQueue::new();
    loop {
        if !queue.enqueue() || !queue.dequeue() {
            break;
        }
    }
}

#[entry]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());

    inner();
    panic!("miscompilation")
}
