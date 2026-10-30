# Adding support for a new board

To add support for a new board, one should implement a HAL (hardware abstraction layer) for this board.
Behind this fancy name is a simple Python module with two functions:

 - initialize_display()
 - show_info(items: dict)
 - read_button() -> bool

## initialize_display

The `initialize_display()` function should perform all the necessary setup to configure the display: setup pins, configure the corresponding bus (SPI, I2C, etc).

Recommended way is to use Adafruit's [displayio](https://docs.circuitpython.org/en/latest/shared-bindings/displayio/) library for the display output, but it is not required and any other approach will work.

## show_info

The `show_info(info: dict)` function is the workhorse of the autospot companion. It takes a dictionary of the current autospot state and converts it into an image on the display. It can use whatever is necessary to display the information - it must adapt the information to the display's size, colour and drawing capabilities.

The input `info` dictionary will have at least one element: `status`. `info['status']` is a string and can have one of the following values:
 - "connected" - the main computer is connected to a network
 - "hotspot" - the main computer is in hotspot mode
 - "disconnected" - the main computer is not connected to any network, but the hotspot is not yet active
 - "wifi-off" - the Wi-Fi has been turned off
 - "message" - display a message on the screen
 - anything else is treated as "unknown state"

Depending on the status, the `info` dict will contain additional keys.

## read_button

Some companion devices have buttons and one of these buttons can be used to toggle the computer between Wi-Fi connection and Hotspot. This can be useful if the device has connected to Wi-Fi to which you have no access and you need to enable Hotspot to reconfigure the computer.

The `read_button` function must do one thing only - return `True` if the toggle button was pressed and released. The implementation details doesn't matter - it can be a hardware button or touchscreen event. The HAL layer must take care of debouncing and prevent repeated button activations. 

See the following example from the T-Dongle S3 implementation which uses built-in `keypad` module to handle press/release events and debouncing

```python

import board
import keypad

_toggle_button = None

def initialize_display():
    # ....
    # Display initialization
    # .....

    global _toggle_button
    _toggle_button = keypad.Keys((board.BUTTON0,), value_when_pressed=False, pull=True)


def read_button():
    # If the button was not initialized, ignore it
    if _toggle_button is None:
        return False
    event = _toggle_button.events.get()
    if event and event.released
        # Only treat the button as pressed after it was released
        _button_pressed = False
        return True
    return False
```

### Connected status

Available keys:
 - "ssid" - the name of the WiFi network the computer is connected to, or, "\<ethernet\>" if the connection is wired.
 - "ip_address" - the IP address of the computer

### Hotspot status

Available keys:
 - "ssid" - the name of the created Hotspot network
 - "password" - the password to the hotspot network
 - "ip_address" - the IP address of the computer

### Disconnected status

 - "time" - the time in seconds since disconnect. This key is optional, the code must handle the absense of this key.

### Message statuis
 
 - "message" - the string to display

### Wifi Off status

No additional keys are available for this status.

### Unknown status

No additional keys are available for this status.

## Implementation notes

The code runs constantly and updates the screen every several seconds. HAL writer should take care to ensure that display is refreshed without unnecessary flickering.

When implementing the `show_info` function, keep in mind the use case - indicating network status of the NINA computer to astrophotographers. Display information in the simplest and most readable format, and use red colour to preserve the user's night vision. Avoid bright lights (i.e. don't use bright LEDs even if the board has the best LEDs in the world).

### License

If you want the code for your board to be included in the Autospot GitHub repo, your contribution must be licensed under the terms of GPL-3 license. 

## Example

For the example, please refer to [LilyGo T-Display RP2040 HAL implementation](LilyGo%20T-Display%20RP2040/hal.py)
