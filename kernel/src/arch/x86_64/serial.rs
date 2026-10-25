//! Bounded, allocation-free COM1 output for the single-CPU, interrupt-free boot.

use core::{arch::asm, fmt};

const COM1: u16 = 0x3f8;
const POLL_LIMIT: usize = 4096;

// SAFETY for all port operations: callers execute at Ring 0 on the configured
// QEMU PC platform. This module exclusively owns COM1; interrupts stay disabled.
unsafe fn out(offset: u16, value: u8) {
    unsafe {
        asm!("out dx, al", in("dx") COM1 + offset, in("al") value, options(nomem, nostack, preserves_flags))
    }
}

unsafe fn input(offset: u16) -> u8 {
    let value: u8;
    unsafe {
        asm!("in al, dx", in("dx") COM1 + offset, out("al") value, options(nomem, nostack, preserves_flags))
    }
    value
}

pub fn initialize() -> Result<(), &'static str> {
    // SAFETY: Only neu_main calls initialization, before any normal output.
    unsafe {
        out(1, 0); // Disable UART interrupts.
        out(3, 0x80); // Divisor latch access.
        out(0, 1); // 115200 baud.
        out(1, 0);
        out(3, 3); // Eight data bits, no parity, one stop bit.
        out(2, 0xc7); // Enable and clear FIFOs.
        out(4, 0x1e); // Internal loopback; do not emit the test byte externally.
        out(0, 0xae);
        let mut received = false;
        for _ in 0..POLL_LIMIT {
            if input(5) & 1 != 0 {
                received = input(0) == 0xae;
                break;
            }
        }
        out(4, 0x0b); // Normal operation, DTR/RTS/OUT2.
        if !received {
            return Err("COM1 loopback failed");
        }
    }
    Ok(())
}

struct Writer {
    remaining: usize,
}

impl Writer {
    fn byte(&mut self, byte: u8) -> fmt::Result {
        if self.remaining == 0 {
            return Err(fmt::Error);
        }
        self.remaining -= 1;
        // SAFETY: No other CPU/interrupt writer exists. Emergency output has no
        // locks and can interrupt formatting; a recursive panic then halts.
        unsafe {
            for _ in 0..POLL_LIMIT {
                if input(5) & 0x20 != 0 {
                    out(0, byte);
                    return Ok(());
                }
                core::hint::spin_loop();
            }
        }
        self.remaining = 0;
        Err(fmt::Error)
    }
}

impl fmt::Write for Writer {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for byte in text.bytes() {
            if byte == b'\n' {
                self.byte(b'\r')?;
            }
            self.byte(byte)?;
        }
        Ok(())
    }
}

pub fn write(arguments: fmt::Arguments<'_>) -> fmt::Result {
    use fmt::Write;
    Writer { remaining: 1024 }.write_fmt(arguments)
}

/// Best effort even if initialization or normal formatting failed. At most
/// 512 bytes, 4096 status polls per byte, no heap, locks, or scheduler operations.
pub fn emergency(arguments: fmt::Arguments<'_>) {
    use fmt::Write;
    let _ = Writer { remaining: 512 }.write_fmt(arguments);
}
