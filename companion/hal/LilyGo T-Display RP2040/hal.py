import displayio
import board
import busio
import fourwire
import adafruit_st7789
import terminalio
from adafruit_display_text import label

SCALE = 2
_main_group = None
_display = None

RED = 0xFF0000

def _clear_group():
    if _main_group != None:
        while len(_main_group) > 0:
            _main_group.pop()

def _add_line(text, y):
    _main_group.append(label.Label(terminalio.FONT, text=text, color=RED, x=4, y=y, scale=SCALE))


def show_info(items):
    _clear_group()

    if items['status'] == "connected":
        _add_line("Network connected", 20)
        _add_line("SSID:", 55)
        _add_line(items["ssid"], 80)
        _add_line("IP: " + items["ip_address"], 105)
    elif items['status'] == "hotspot":
        _add_line("Hotspot enabled", 20)
        _add_line("SSID: " + items["ssid"], 45)
        _add_line("Password: ",70)
        _add_line(items["password"], 95)
        _add_line("IP: " + items["ip_address"], 120)
    elif items['status'] == "disconnected":
        _add_line("   Disconnected", 65)
    else:
        _add_line("   Unknown state", 65)
    _display.refresh()


def initialize_display():

    displayio.release_displays()

    tft_clk = board.LCD_CLK
    tft_mosi = board.LCD_MOSI
    tft_cs = board.LCD_CS
    tft_dc = board.LCD_DC
    tft_rst = board.LCD_RESET
    tft_bl = board.LCD_BACKLIGHT

    tft_spi = busio.SPI(clock=tft_clk, MOSI=tft_mosi)
    display_bus = fourwire.FourWire(tft_spi, command=tft_dc, chip_select=tft_cs, reset=tft_rst)
    global _display, _main_group
    _display = adafruit_st7789.ST7789(display_bus,
                                    width=135, height=240,
                                    rowstart=40, colstart=53,
                                    backlight_pin=tft_bl)
    _display.rotation=270
    _display.auto_refresh = False
    _main_group = displayio.Group()
    _display.root_group = _main_group
