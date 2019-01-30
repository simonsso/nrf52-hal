# spi-demo
SPIM demonstation code.
Connect a resistor between pin 22 and 23 on to feed MOSI direct back to MISO
If all tests Led1 to 4 will light up, in case of error only the failing test
case will light up.

## HW connections
Pin     Connecton   
P0.24   SPIclk
P0.23   MOSI
P0.22   MISO

This is designed for nRF52-DK board:
https://www.nordicsemi.com/Software-and-Tools/Development-Kits/nRF52-DK
