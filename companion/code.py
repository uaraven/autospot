# SPDX-License-Identifier: GPL-3.0-or-later
# Part of the Autospot companion; see LICENSE.

import json
import board
import busio, digitalio
import displayio
import fourwire
import vectorio, rainbowio
import terminalio
import adafruit_st7789
from adafruit_display_text import label

displayio.release_displays()


def read_status():
    try:
        with open("status.json", "r") as f:
            return json.load(f)
    except Exception as e:
        print("Error reading status.json:", e)
        return {"status": "unknown"}
    
# print(dir(board))

tft_pwr = board.TFT_POWER
tft_clk = board.LCD_CLK
tft_mosi = board.LCD_MOSI
tft_cs = board.LCD_CS
tft_dc = board.LCD_DC
tft_rst = board.LCD_RESET
tft_bl = board.LCD_BACKLIGHT

tft_spi = busio.SPI(clock=tft_clk, MOSI=tft_mosi)
display_bus = fourwire.FourWire(tft_spi, command=tft_dc, chip_select=tft_cs, reset=tft_rst)
display = adafruit_st7789.ST7789(display_bus,
                                 width=135, height=240,
                                 rowstart=40, colstart=53,
                                 backlight_pin=tft_bl)
display.rotation=270
maingroup = displayio.Group()
display.root_group = maingroup

RED = 0xFF0000

def clear_group():
    while len(maingroup) > 0:
        maingroup.pop()


def add_line(text, y):
    maingroup.append(label.Label(terminalio.FONT, text=text, color=RED, x=4, y=y, scale=2))


def display_wifi_info(wifi):
    clear_group()
    add_line("Network connected", 20)
    add_line("SSID:", 55)
    add_line(wifi["ssid"], 80)
    add_line("IP: " + wifi["ip_address"], 105)


def display_hotspot_info(hotspot):
    clear_group()
    add_line("Hotspot enabled", 20)
    add_line("SSID: " + hotspot["ssid"], 45)
    add_line("Pass: ",70)
    add_line(hotspot["password"], 95)
    add_line("IP: " + hotspot["ip_address"], 120)


def display_no_wifi():
    clear_group()
    add_line("   Disconnected", 65)

def display_unknown_state():
    clear_group()
    add_line("   Unknown state", 65)


status = read_status()
if status["status"] == "connected":
    display_wifi_info(status["connected"])
elif status["status"] == "hotspot":
    display_hotspot_info(status["hotspot"])
elif status["status"] == "disconnected":
    display_no_wifi()
else:
    display_unknown_state()

while True:
    pass