# Autospot

Automatic Hot-Spot creator for Windows.

This program is intended for astrophotographers who use their NINA computer both in the home network and in the field. 

Autospot starts on Windows startup and constantly monitors connections. If the computer loses WiFi connection, it waits for 2 minutes for reconnection and then creates a hotspot.

## Installation

There is no installation. autospot is a portable exe file that just needs to be started.

Unpack the zip file, change the configuration according to your needs and run the program.

To enable automatic start with windows:
 - In the explorer context menu for autospot.exe, choose "Create shortcut"
 - Press Windows Key+R and run `shell:startup` - this will open the startup folder
 - Move the shortcut for autospot.exe into the Startup folder.
 - autospot.exe will start the next time you restart windows.

## Configuration

There is a `autospot.toml` configuration file distributed alongside the `autospot.exe`. Edit it with notepad to update the settings as you need. You can change the check interval, the disconnection wait time, name and password for the hotspot, etc. All the configuration options in `autospot.toml` are commented with explanations.

## Companion device

Companion device is a DIY USB dongle running a microcontroller with a display. Companion simplifies astrophotographer's life (already complicated without all this IT shit) by showing the current connection status of the NINA computer along with IP address for remote desktop connection and hotspot ssid and password if hotspot is enabled.

See the [companion's readme](companion/readme.md) for the details. 


# License

Autospot is released into the public domain under the [Unlicense](LICENSE). Do whatever you want with it.