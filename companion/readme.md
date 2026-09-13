# Autospot USB companion

This is the source code for the Autospot companion. This code runs on a CircuitPython-compatible microcontroller, connected to the host computer by USB and displaying the network connection information.

![](companion.jpg)

## Requirements

Microcontroller that is compatible with CircuitPython 10. has a display and presents itself as a USB mass-storage device. Examples include Lilygo T-Display RP2040, LilyGo T-Dongle S3, etc.

## Installation

Install [CircuitPython](https://circuitpython.org/downloads) version 10 onto your microcontroller and then copy `code.py` file and `lib` folder to the CIRCUITPY drive.

Run autospot - the status of the connection should be displayed on the controller's display

## License

This companion is licensed under the GNU General Public License v3.0 (GPLv3) -- see
[LICENSE](LICENSE). 

