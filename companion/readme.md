# Autospot USB companion

Autospot companion is a DIY USB dongle based on a microcontroller with a display and a USB port.

Autospot companion plugs into the computer running [autospot](https://github.com/uaraven/autospot) and shows the current connection status, machine IP address and hotspot name/password if hotspot is available. Note that the companion **requires** running autospot; it doesn't work by itself.

This repository contains the source code for the Autospot companion. This code runs on a CircuitPython-compatible microcontroller, connected to the host computer by USB and displaying the network connection information.

![](companion.jpg)

## Requirements

A microcontroller that is compatible with CircuitPython 10, and has a display. Devices that present an USB mass storage drive are recommended because of the ease of installation.

There is no real requirement to have all-in-one pre-packaged board with display. Adding display (such as [Pimoroni Pico Display](https://shop.pimoroni.com/products/pico-display-pack?variant=32368664215635)) to Raspberry Pi Pico will work just fine.

Examples include LilyGo T-Display RP2040, LilyGo T-Dongle S3, etc.

### Operation

autospot.exe running on the host computer automatically detects the companion device and sends a status update every time when ther e is a change in the network configuration.

Companion device shows the status received from the autospot, including active Wi-Fi connection with the host IP address, name of the hotspot, password and host IP address in the Hotspot mode and others.

There is no way to test all the available devices for compatibility, but most of the RP2040, RP2350 and ESP32-S3 based devices should work. 

Boards that are not listed in the [list below](#supported-boards) can work, but might require some programming to properly support
display. Display bus, initialization, resolution might require writing a new hardware abstraction layer for the board. See [board docs](board/readme.md) for details.

## Installation

Install [CircuitPython](https://circuitpython.org/downloads) version 10 onto your microcontroller and then copy `code.py` and `boot.py` files to the CIRCUITPY drive.
In the `board` directory select the folder corresponding to your microcontroller and copy all files and folders to the CIRCUITPY drive as well. There usually will be `hal.py` and `lib` folders, but there might be other files and folders, copy everything.
**Note**: All the files must be copied to the root of the CIRCUITPY drive. After copying, unmount the disk, disconnect it and then reconnect it back again.

Run autospot - the status of the connection should be displayed on the controller's display.

If autospot's logs shows "no companion device found", you might need to change the configuration of the USB device, refer to [companion installation guide](install.txt) for more details.


## Supported boards

|                                   Board                                   | CircuitPython download                                   | Notes                                        |
| :-----------------------------------------------------------------------: | :------------------------------------------------------- | :------------------------------------------- |
| [LilyGo T-Display RP2040](https://lilygo.cc/collections/t-display-series) | https://circuitpython.org/board/lilygo_t_display_rp2040/ | It looks like LilyGo discontinued this board |
|    [Waveshare RP2350-Geek](https://www.waveshare.com/wiki/RP2350-GEEK)    | https://circuitpython.org/board/waveshare_rp2350_geek/   |                                              |

## Contributing

If you want to add support for a new microcontroller board follow these steps:

- create a directory for your board in `hal` folder
- create `lib` folder and copy any CircuitPython libraries that are needed by the board there
- create `hal.py` file and implement the following functions:
  - initialize_display() - to initialize the display
  - show_info(items: dict) - to display the information
- create pull request

See [this file](hal/readme.md) for more details on HAL implementation.

## License

This companion is licensed under the GNU General Public License v3.0 (GPLv3) -- see
[LICENSE](LICENSE).
