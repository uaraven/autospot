# Adding support for a new board

To add support for a new board, one should implement a HAL (hardware abstraction layer) for this board.
Behind this fancy name is a simple Python module with two functions:

 - initialize_display()
 - show_info(items: dict)

## initialize_display

Autospot companion uses Adafruit's [displayio](https://docs.circuitpython.org/en/latest/shared-bindings/displayio/) library for the display output.

For more information on `displayio` and how to use it refer to this [tutorial](https://learn.adafruit.com/circuitpython-display-support-using-displayio/introduction).

The `initialize_display()` function should perform all the necessary setup to configure the display: setup pins, configure the corresponding bus (SPI, I2C, etc).

## show_info

The `show_info(info: dict)` function is the workhorse of the autospot companion. It takes a dictionary of the current autospot state and converts it into an image on the display. It can use whatever is necessary to display the information - it must adapt the information to the display's size, colour and drawing capabilities.

The input `info` dictionary will have at least one element: `status`. `info['status']` is a string and can have one of the following values:
 - "connected" - the main computer is connected to a network
 - "hotspot" - the main computer is in hotspot mode
 - "disconnected" - the main computer is not connected to any network, but the hotspot is not yet active
 - anything else is treated as "unknown state"

Depending on the status, the `info` dict will contain additional keys.

### Connected status

Available keys:
 - "ssid" - the name of the WiFi network the computer is connected to, or, "\<ethernet\>" if the connection is wired.
 - "ip_address" - the IP address of the computer

### Hotspot status

Available keys:
 - "ssid" - the name of the created Hotspot network
 - "password" - the password to the hotspot network
 - "ip_address" - the IP address of the computer

### Disconnected and unknown statuses

No other keys are available.

## Implementation notes

The code runs only once when the controller starts. Every time the status is updated, the controller is reset and the main code in `code.py` runs again, calling each of the HAL's functions once.

When implementing the `show_info` function, keep in mind the use case - indicating network status of the NINA computer to astrophotographers. Display information in the simplest and most readable format, and use red colour to preserve the user's night vision. Avoid bright lights (i.e. don't use bright LEDs even if the board has the best LEDs in the world).

### License

If you want the code for your board to be included in the Autospot GitHub repo, your contribution must be licensed under the terms of GPL-3 license. 

## Example

For the example, please refer to [LilyGo T-Display RP2040 HAL implementation](LilyGo%20T-Display%20RP2040/hal.py)
