//! HAL interface to the SPIM peripheral
//!
//! See product specification, chapter 31.
use embedded_hal::blocking::spi::Transfer;
use core::ops::Deref;
use core::mem::transmute;
use core::sync::atomic::{compiler_fence, Ordering::AcqRel};
use crate::hal::spi::FullDuplex;


#[macro_use(block)]
use nb::{block};

use crate::target::{
    spim0,
    SPIM0,
    SPIM1,
    SPIM2,
    spi0,
};

use crate::prelude::*;
use crate::gpio::{
    p0::P0_Pin,
    Floating,
    Input,
    Output,
    PushPull,
};

pub trait SpimExt : Deref<Target=spim0::RegisterBlock> + Sized {
    fn constrain(self, pins: Pins) -> Spim<Self>;
}

macro_rules! impl_spim_ext {
    ($($spim:ty,)*) => {
        $(
            impl SpimExt for $spim {
                fn constrain(self, pins: Pins) -> Spim<Self> {
                    Spim::new(self, pins)
                }
            }          
        )*
    }
}

impl_spim_ext!(
    SPIM0,
    SPIM1,
    SPIM2,
);


/// Interface to a SPIM instance
///
/// This is a very basic interface that comes with the following limitations:
/// - The SPIM instances share the same address space with instances of SPIS,
///   SPI, TWIM, TWIS, and TWI. You need to make sure that conflicting instances
///   are disabled before using `Spim`. See product specification, section 15.2.
/// - The SPI mode is hardcoded to SPI mode 0.
/// - The frequency is hardcoded to 500 kHz.
/// - The over-read character is hardcoded to `0`.
pub struct Spim<T>(T);

impl<T> Transfer<u8> for Spim<T> where T: SpimExt

// impl<T> Transfer<u8> for S<T> where
//      T: SpimExt,
//      S: FullDuplex<u8>,
{
   type Error = SPIError;

    fn transfer<'w>(&mut self, words: &'w mut [u8]) -> Result<&'w [u8], SPIError> {
        for word in words.iter_mut() {
            block!(self.send(word.clone()))?;
            *word = block!(FullDuplex::read(self))?;
        }

        Ok(words)
    }
}

impl<T> FullDuplex<u8> for Spim<T> where T: SpimExt
{
    type Error = SPIError;
    fn read(&mut self) -> nb::Result<u8, Self::Error>{
        unsafe{
            //  Spi0 and Spim0 is the same hw on the same address, they are split in register map
            //  but will work fine together This will cast a spim0 to spi0 to access registers.
            let spi:*const spi0::RegisterBlock = transmute(self.0.deref());
            if (*spi).events_ready.read().bits() == 1 {
                Err(nb::Error::WouldBlock)
            }else{
                Ok(((*spi).rxd.read().bits() & 0xFF )as u8)
            }
        }
    }
    fn send(&mut self, word: u8) -> nb::Result<(), Self::Error>{

        unsafe {
            //  Spi0 and Spim0 is the same hw on the same address, they are split in register map
            //  but will work fine together This will cast a spim0 to spi0 to access registers.
            let spi:*const spi0::RegisterBlock =  transmute(self.0.deref())   ;
            if (*spi).events_ready.read().bits() == 0 {
                Err(nb::Error::WouldBlock)
            }else{
                (*spi).txd.write(|w|{ w.txd().bits(word)});
                Ok(())
            }
        }

    }

}
impl<T> Spim<T> where T: SpimExt {
    pub fn new(spim: T, pins: Pins) -> Self {
        // Select pins
        spim.psel.sck.write(|w| {
            let w = unsafe { w.pin().bits(pins.sck.pin) };
            w.connect().connected()
        });
        spim.psel.mosi.write(|w| {
            let w = unsafe { w.pin().bits(pins.mosi.pin) };
            w.connect().connected()
        });
        spim.psel.miso.write(|w| {
            let w = unsafe { w.pin().bits(pins.miso.pin) };
            w.connect().connected()
        });

        // Enable SPIM instance
        spim.enable.write(|w|
            w.enable().enabled()
        );

        // Set to SPI mode 0
        spim.config.write(|w|
            w
                .order().msb_first()
                .cpha().leading()
                .cpol().active_high()
        );

        // Configure frequency
        spim.frequency.write(|w|
            w.frequency().k500() // 500 kHz
        );

        // Set over-read character to `0`
        spim.orc.write(|w|
            // The ORC field is 8 bits long, so `0` is a valid value to write
            // there.
            unsafe { w.orc().bits(0) }
        );

        Spim(spim)
    }

    /// Read from an SPI slave
    ///
    /// This method implements a complete read transaction, which consists of
    /// the master transmitting what it wishes to read, and the slave responding
    /// with the requested data.
    ///
    /// Uses the provided chip select pin to initiate the transaction. Transmits
    /// all bytes in `tx_buffer`, then receives bytes until `rx_buffer` is full.
    /// Both buffer must have a length of at most 255 bytes.
    pub fn read(&mut self,
        chip_select: &mut P0_Pin<Output<PushPull>>,
        tx_buffer  : &[u8],
        rx_buffer  : &mut [u8],
    )
        -> Result<(), SPIError>
    {
        // TODO: some targets have a maxcnt whose size is larger
        // than a u8, so this length check is overly restrictive
        // and could be lifted.
        if tx_buffer.len() > u8::max_value() as usize {
            return Err(SPIError::TxBufferTooLong);
        }
        if rx_buffer.len() > u8::max_value() as usize {
            return Err(SPIError::RxBufferTooLong);
        }

        // Pull chip select pin high, which is the inactive state
        chip_select.set_high();

        // Set up the DMA write
        self.0.txd.ptr.write(|w|
            // We're giving the register a pointer to the stack. Since we're
            // waiting for the SPI transaction to end before this stack pointer
            // becomes invalid, there's nothing wrong here.
            //
            // The PTR field is a full 32 bits wide and accepts the full range
            // of values.
            unsafe { w.ptr().bits(tx_buffer.as_ptr() as u32) }
        );
        self.0.txd.maxcnt.write(|w|
            // We're giving it the length of the buffer, so no danger of
            // accessing invalid memory. We have verified that the length of the
            // buffer fits in an `u8`, so the cast to the type of maxcnt
            // is also fine.
            //
            // Note that that nrf52840 maxcnt is a wider
            // type than a u8, so we use a `_` cast rather than a `u8` cast.
            // The MAXCNT field is thus at least 8 bits wide and accepts the full
            // range of values that fit in a `u8`.
            unsafe { w.maxcnt().bits(tx_buffer.len() as _) }
        );

        // Set up the DMA read
        self.0.rxd.ptr.write(|w|
            // This is safe for the same reasons that writing to TXD.PTR is
            // safe. Please refer to the explanation there.
            unsafe { w.ptr().bits(rx_buffer.as_mut_ptr() as u32) }
        );
        self.0.rxd.maxcnt.write(|w|
            // This is safe for the same reasons that writing to TXD.MAXCNT is
            // safe. Please refer to the explanation there.
            unsafe { w.maxcnt().bits(rx_buffer.len() as _) }
        );

        // Start SPI transaction
        chip_select.set_low();
        self.0.tasks_start.write(|w|
            // `1` is a valid value to write to task registers.
            unsafe { w.bits(1) }
        );

        // Wait for END event
        //
        // This event is triggered once both transmitting and receiving are
        // done.
        while self.0.events_end.read().bits() == 0 {}

        // Reset the event, otherwise it will always read `1` from now on.
        self.0.events_end.write(|w| w);

        // End SPI transaction
        chip_select.set_high();

        if self.0.txd.amount.read().bits() != tx_buffer.len() as u32 {
            return Err(SPIError::Transmit);
        }
        if self.0.rxd.amount.read().bits() != rx_buffer.len() as u32 {
            return Err(SPIError::Receive);
        }

        // Conservative compiler fence to prevent optimizations that do not
        // take in to account DMA
        compiler_fence(AcqRel);

        Ok(())
    }

    /// Write to an SPI slave
    ///
    /// This method uses the provided chip select pin to initiate the
    /// transaction, then transmits all bytes in `tx_buffer`.
    ///
    /// The buffer must have a length of at most 255 bytes.
    pub fn write(&mut self,
        chip_select: &mut P0_Pin<Output<PushPull>>,
        tx_buffer  : &[u8],
    )
        -> Result<(), SPIError>
    {
        // This is overly restrictive. See:
        // https://github.com/nrf-rs/nrf52/issues/17
        if tx_buffer.len() > u8::max_value() as usize {
            return Err(SPIError::TxBufferTooLong);
        }

        // Pull chip select pin high, which is the inactive state
        chip_select.set_high();

        // Set up the DMA write
        self.0.txd.ptr.write(|w|
            // We're giving the register a pointer to the stack. Since we're
            // waiting for the SPI transaction to end before this stack pointer
            // becomes invalid, there's nothing wrong here.
            //
            // The PTR field is a full 32 bits wide and accepts the full range
            // of values.
            unsafe { w.ptr().bits(tx_buffer.as_ptr() as u32) }
        );
        self.0.txd.maxcnt.write(|w|
            // We're giving it the length of the buffer, so no danger of
            // accessing invalid memory. We have verified that the length of the
            // buffer fits in an `u8`, so the cast to `u8` is also fine.
            //
            // The MAXCNT field is 8 bits wide and accepts the full range of
            // values.
            unsafe { w.maxcnt().bits(tx_buffer.len() as _) }
        );

        // Tell the RXD channel it doesn't need to read anything
        self.0.rxd.maxcnt.write(|w|
            // This is safe for the same reasons that writing to TXD.MAXCNT is
            // safe. Please refer to the explanation there.
            unsafe { w.maxcnt().bits(0) }
        );

        // Start SPI transaction
        chip_select.set_low();
        self.0.tasks_start.write(|w|
            // `1` is a valid value to write to task registers.
            unsafe { w.bits(1) }
        );

        // Wait for transmission to end
        while self.0.events_end.read().bits() == 0 {}

        // Reset the event, otherwise it will always read `1` from now on.
        self.0.events_end.write(|w| w);

        // End SPI transaction
        chip_select.set_high();

        if self.0.txd.amount.read().bits() != tx_buffer.len() as u32 {
            return Err(SPIError::Transmit);
        }

        // Conservative compiler fence to prevent optimizations that do not
        // take in to account DMA
        compiler_fence(AcqRel);

        Ok(())
    }

    /// Return the raw interface to the underlying SPIM peripheral
    pub fn free(self) -> T {
        self.0
    }
}


pub struct Pins {
    // SPI clock
    pub sck: P0_Pin<Output<PushPull>>,

    // Master out, slave in
    pub mosi: P0_Pin<Output<PushPull>>,

    // Master in, slave out
    pub miso: P0_Pin<Input<Floating>>,
}


#[derive(Debug)]
pub enum SPIError {
    TxBufferTooLong,
    RxBufferTooLong,
    Transmit,
    Receive,
}
