# Autospot

Automatic Hot-Spot creator for Windows.

This program is intended for astrophotographers who use their NINA computer both in the home network and in the field. 

Autospot starts on Windows startup and constantly monitors connections. If the computer loses WiFi connection, it waits for 2 minutes for reconnection and then creates a hotspot.

## Installation

There is no installation. autospot is an portable exe file that just needs to be started.

Unpack the zip file, change the configuration according to your needs and run the program.

## Configuration

There is a `autospot.toml` configuration file distributed alongside the `autospot.exe`. Edit it with notepad to update the settings as you need. You can change the check interval, the disconnection wait time, name and password for the hotspot, etc. All the configuration options in `autospot.toml` are commented with explanations.

## Companion device

See [companion/readme.md](companion/readme.md) for the DIY USB display companion that runs on a CircuitPython microcontroller. 

# License

Autospot is released into the public domain under the [Unlicense](LICENSE). Do whatever you want with it.