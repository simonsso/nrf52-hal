//! HAL interface to the SPI peripheral
//!
//! See product specification:
//!
//! - nrf52832: Section XXX
//! - nrf52840: Section XXX
use core::ops::Deref;
use core::sync::atomic::{compiler_fence, Ordering::AcqRel};

use crate::target::{
    spi0,
    P0,
    SPI0,
    SPI1,
};

use crate::gpio::{
    p0::P0_Pin,
    Floating,
    Input,
};


pub use crate::target::spi0::frequency::FREQUENCYW as Frequency;


pub trait SpiExt: Deref<Target=spi0::RegisterBlock> + Sized {
    fn constrain(self, pins: Pins, frequency: Frequency)
        -> Spi<Self>;
}

macro_rules! impl_spi_ext {
    ($($spi:ty,)*) => {
        $(
            impl SpiExt for $spi {
                fn constrain(self, pins: Pins, frequency: Frequency)
                    -> Spi<Self>
                {
                    Spi::new(self, pins, frequency)
                }
            }
        )*
    }
}

impl_spi_ext!(
    SPI0,
    SPI1,
);


/// Interface to a SPI instance
///
/// This is a very basic interface that comes with the following limitations:
/// - The SPIM instances share the same address space with instances of SPIS,
///   SPI, TWIM, TWIS, and TWI. You need to make sure that conflicting instances
///   are disabled before using `Spim`. See product specification, section 15.2.

pub struct Spi<T>(T);

impl<T> Spi<T> where T: SpiExt {
    pub fn new(spi: T, pins: Pins, frequency: Frequency) -> Self {
        // The SPI peripheral requires the pins to be in a mode that is not
        // exposed through the GPIO API, and might it might not make sense to
        // expose it there.
        //
        // Until we've figured out what to do about this, let's just configure
        // the pins through the raw peripheral API. All of the following is
        // safe, as we own the pins now and have exclusive access to their
        // registers.
        for &pin in &[pins.scl.pin, pins.sda.pin] {
            unsafe { &*P0::ptr() }.pin_cnf[pin as usize].write(|w|
                w
                    .dir().input()
                    .input().connect()
                    .pull().pullup()
                    .drive().s0d1()
                    .sense().disabled()
            );
        }

        // Select pins
        spi.psel.sck.write(|w| {
            let w = unsafe { w.pin().bits(pins.sck.pin) };
            w.connect().connected()
        });
        spi.psel.mosi.write(|w| {
            let w = unsafe { w.pin().bits(pins.mosi.pin) };
            w.connect().connected()
        });
        spi.psel.miso.write(|w| {
            let w = unsafe { w.pin().bits(pins.miso.pin) };
            w.connect().connected()
        });

        // Enable SPI instance
        spi.enable.write(|w|
            w.enable().enabled()
        );

        // Set to SPI mode 0
        spi.config.write(|w|
            w
                .order().msb_first()
                .cpha().leading()
                .cpol().active_high()
        );
        // Configure frequency
        spi.frequency.write(|w| w.frequency().variant(frequency));


        Spi(spi)
    }

    /// Write to an SPI slave
    ///
    /// The buffer must have a length of at most 255 bytes.
    pub fn write(&mut self,
        address: u8,
        buffer:  &[u8],
    )
        -> Result<(), Error>
    {
        // This is overly restrictive. See:
        // https://github.com/nrf-rs/nrf52-hal/issues/17
        if buffer.len() > u8::max_value() as usize {
            return Err(Error::BufferTooLong);
        }

        self.0.address.write(|w| unsafe { w.address().bits(address) });

        // Set up the DMA write
        self.0.txd.ptr.write(|w|
            // We're giving the register a pointer to the stack. Since we're
            // waiting for the I2C transaction to end before this stack pointer
            // becomes invalid, there's nothing wrong here.
            //
            // The PTR field is a full 32 bits wide and accepts the full range
            // of values.
            unsafe { w.ptr().bits(buffer.as_ptr() as u32) }
        );
        self.0.txd.maxcnt.write(|w|
            // We're giving it the length of the buffer, so no danger of
            // accessing invalid memory. We have verified that the length of the
            // buffer fits in an `u8`, so the cast to `u8` is also fine.
            //
            // The MAXCNT field is 8 bits wide and accepts the full range of
            // values.
            unsafe { w.maxcnt().bits(buffer.len() as _) }
        );

        // Start write operation
        self.0.tasks_starttx.write(|w|
            // `1` is a valid value to write to task registers.
            unsafe { w.bits(1) }
        );

        // Wait until write operation is about to end
        while self.0.events_lasttx.read().bits() == 0 {}
        self.0.events_lasttx.write(|w| w); // reset event

        // Stop read operation
        self.0.tasks_stop.write(|w|
            // `1` is a valid value to write to task registers.
            unsafe { w.bits(1) }
        );

        // Wait until write operation has ended
        while self.0.events_stopped.read().bits() == 0 {}
        self.0.events_stopped.write(|w| w); // reset event

        if self.0.txd.amount.read().bits() != buffer.len() as u32 {
            return Err(Error::Transmit);
        }

        // Conservative compiler fence to prevent optimizations that do not
        // take in to account DMA
        compiler_fence(AcqRel);

        Ok(())
    }

    /// Read from an I2C slave
    pub fn read(&mut self,
        address: u8,
        buffer:  &mut [u8],
    )
        -> Result<(), Error>
    {
        // This is overly restrictive. See:
        // https://github.com/nrf-rs/nrf52-hal/issues/17
        if buffer.len() > u8::max_value() as usize {
            return Err(Error::BufferTooLong);
        }

        self.0.address.write(|w| unsafe { w.address().bits(address) });

        // Set up the DMA read
        self.0.rxd.ptr.write(|w|
            // We're giving the register a pointer to the stack. Since we're
            // waiting for the I2C transaction to end before this stack pointer
            // becomes invalid, there's nothing wrong here.
            //
            // The PTR field is a full 32 bits wide and accepts the full range
            // of values.
            unsafe { w.ptr().bits(buffer.as_mut_ptr() as u32) }
        );
        self.0.rxd.maxcnt.write(|w|
            // We're giving it the length of the buffer, so no danger of
            // accessing invalid memory. We have verified that the length of the
            // buffer fits in an `u8`, so the cast to the type of maxcnt
            // is also fine.
            //
            // Note that that nrf52840 maxcnt is a wider
            // type than a u8, so we use a `_` cast rather than a `u8` cast.
            // The MAXCNT field is thus at least 8 bits wide and accepts the
            // full range of values that fit in a `u8`.
            unsafe { w.maxcnt().bits(buffer.len() as _) }
        );

        // Start read operation
        self.0.tasks_startrx.write(|w|
            // `1` is a valid value to write to task registers.
            unsafe { w.bits(1) }
        );

        // Wait until read operation is about to end
        while self.0.events_lastrx.read().bits() == 0 {}
        self.0.events_lastrx.write(|w| w); // reset event

        // Stop read operation
        self.0.tasks_stop.write(|w|
            // `1` is a valid value to write to task registers.
            unsafe { w.bits(1) }
        );

        // Wait until read operation has ended
        while self.0.events_stopped.read().bits() == 0 {}
        self.0.events_stopped.write(|w| w); // reset event

        if self.0.rxd.amount.read().bits() != buffer.len() as u32 {
            return Err(Error::Receive);
        }

        // Conservative compiler fence to prevent optimizations that do not
        // take in to account DMA
        compiler_fence(AcqRel);

        Ok(())
    }

    /// Write data to an I2C slave, then read data from the slave without
    /// triggering a stop condition between the two
    ///
    /// The buffer must have a length of at most 255 bytes.
    pub fn write_then_read(&mut self,
        address: u8,
        wr_buffer:  &[u8],
        rd_buffer: &mut [u8],
    )
        -> Result<(), Error>
    {
        // This is overly restrictive. See:
        // https://github.com/nrf-rs/nrf52-hal/issues/17
        if wr_buffer.len() > u8::max_value() as usize {
            return Err(Error::BufferTooLong);
        }

        if rd_buffer.len() > u8::max_value() as usize {
            return Err(Error::BufferTooLong);
        }

        self.0.address.write(|w| unsafe { w.address().bits(address) });

        // Set up the DMA write
        self.0.txd.ptr.write(|w|
            // We're giving the register a pointer to the stack. Since we're
            // waiting for the I2C transaction to end before this stack pointer
            // becomes invalid, there's nothing wrong here.
            //
            // The PTR field is a full 32 bits wide and accepts the full range
            // of values.
            unsafe { w.ptr().bits(wr_buffer.as_ptr() as u32) }
        );
        self.0.txd.maxcnt.write(|w|
            // We're giving it the length of the buffer, so no danger of
            // accessing invalid memory. We have verified that the length of the
            // buffer fits in an `u8`, so the cast to `u8` is also fine.
            //
            // The MAXCNT field is 8 bits wide and accepts the full range of
            // values.
            unsafe { w.maxcnt().bits(wr_buffer.len() as _) }
        );

        // Set up the DMA read
        self.0.rxd.ptr.write(|w|
            // We're giving the register a pointer to the stack. Since we're
            // waiting for the I2C transaction to end before this stack pointer
            // becomes invalid, there's nothing wrong here.
            //
            // The PTR field is a full 32 bits wide and accepts the full range
            // of values.
            unsafe { w.ptr().bits(rd_buffer.as_mut_ptr() as u32) }
        );
        self.0.rxd.maxcnt.write(|w|
            // We're giving it the length of the buffer, so no danger of
            // accessing invalid memory. We have verified that the length of the
            // buffer fits in an `u8`, so the cast to the type of maxcnt
            // is also fine.
            //
            // Note that that nrf52840 maxcnt is a wider
            // type than a u8, so we use a `_` cast rather than a `u8` cast.
            // The MAXCNT field is thus at least 8 bits wide and accepts the
            // full range of values that fit in a `u8`.
            unsafe { w.maxcnt().bits(rd_buffer.len() as _) }
        );

        // Immediately start RX after TX, then stop
        self.0.shorts.modify(|_r, w|
            w.lasttx_startrx().enabled()
             .lastrx_stop().enabled()
        );

        // Start write operation
        self.0.tasks_starttx.write(|w|
            // `1` is a valid value to write to task registers.
            unsafe { w.bits(1) }
        );

        // Wait until total operation has ended
        while self.0.events_stopped.read().bits() == 0 {}

        self.0.events_lasttx.write(|w| w); // reset event
        self.0.events_lastrx.write(|w| w); // reset event
        self.0.events_stopped.write(|w| w); // reset event
        self.0.shorts.write(|w| w);

        let bad_write = self.0.txd.amount.read().bits() != wr_buffer.len() as u32;
        let bad_read  = self.0.rxd.amount.read().bits() != rd_buffer.len() as u32;

        // Conservative compiler fence to prevent optimizations that do not
        // take in to account DMA
        compiler_fence(AcqRel);

        if bad_write {
            return Err(Error::Transmit);
        }

        if bad_read {
            return Err(Error::Receive);
        }

        Ok(())
    }

    /// Return the raw interface to the underlying SPI peripheral
    pub fn free(self) -> T {
        self.0
    }
}


/// The pins used by the SPI peripheral
///
/// Currently, only P0 pins are supported.
pub struct Pins {
    // SPI clock
    pub sck: P0_Pin<Output<PushPull>>,

    // Master out, slave in
    pub mosi: P0_Pin<Output<PushPull>>,

    // Master in, slave out
    pub miso: P0_Pin<Input<Floating>>,
}


#[derive(Debug)]
pub enum Error {
    TxBufferTooLong,
    RxBufferTooLong,
    Transmit,
    Receive,
}
