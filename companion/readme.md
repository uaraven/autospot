# Autospot USB companion

Autospot companion is a DIY USB dongle based on a microcontroller with a display and a USB port.

This repository contains the source code for the Autospot companion. This code runs on a CircuitPython-compatible microcontroller, connected to the host computer by USB and displaying the network connection information.

![](companion.jpg)

## Requirements

A microcontroller that is compatible with CircuitPython 10, has a display, and presents itself as a USB mass-storage device. Examples include Lilygo T-Display RP2040, LilyGo T-Dongle S3, etc.

### Why USB mass-storage?

The companion code is as simple as it gets - it reads the file from the storage and displays its contents. Autospot does not support Serial-over-USB communications, it only writes JSON to the specified drive.

CircuitPython automatically presents the microcontroller's flash memory as a USB drive for the _compatible_ devices. Usually, "compatible" means a device that can manage USB connection on its own. A lot of popular ESP-based microcontrollers are not supported, as they require a separate chip for USB connection.

I recommend RP2040-based devices, they are cheap, support USB and are compatible with CircuitPython. One can get a Lilygo T-Display RP2040 for $20 with enclosure and that's all you need.
ESP32-S3 (but not C-series) based devices should also work.

## Installation

Install [CircuitPython](https://circuitpython.org/downloads) version 10 onto your microcontroller and then copy `code.py` file and `lib` folder to the CIRCUITPY drive.

Run autospot - the status of the connection should be displayed on the controller's display. If you change the drive label from CIRCUITPY to something else, don't forget to edit autospot.toml file and update the label name.

## License

This companion is licensed under the GNU General Public License v3.0 (GPLv3) -- see
[LICENSE](LICENSE). 

