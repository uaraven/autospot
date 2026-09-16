import displayio
import board
import busio
import fourwire
import adafruit_st7789
import terminalio
import vectorio
from adafruit_display_text import label


SCALE = 2
_main_group = None
_display = None

RED = 0xFF0000
wifi_bitmap = displayio.OnDiskBitmap("/images/wifi.bmp")
addr_bitmap = displayio.OnDiskBitmap("/images/address.bmp")
disc_bitmap = displayio.OnDiskBitmap("/images/disconnected.bmp")
hotspot_bitmap = displayio.OnDiskBitmap("/images/hotspot.bmp")
nodata_bitmap = displayio.OnDiskBitmap("/images/no-data.bmp")
pass_bitmap = displayio.OnDiskBitmap("/images/lock.bmp")
clock_bitmap = displayio.OnDiskBitmap("/images/clock.bmp")

def _clear_group():
    if _main_group != None:
        while len(_main_group) > 0:
            _main_group.pop()

def _add_line(text, y):
    _main_group.append(label.Label(terminalio.FONT, text=text, color=RED, x=4, y=y, anchor_point=(0,0), scale=SCALE))


def _icon(bmp, x,y):
    icon_tile = displayio.TileGrid(bmp,
        pixel_shader=bmp.pixel_shader,
        x=x,  # X position on screen
        y=y   # Y position on screen
    )
    return icon_tile


def _ico_line(icon_bmp, text, y):
    _main_group.append(_icon(icon_bmp, 0, y))
    _main_group.append(label.Label(terminalio.FONT, text=text, color=RED, x=30, y=y+12, anchor_point=(0,0.5), scale=SCALE))


def show_info(items):
    _clear_group()

    if items['status'] == "connected":
        _ico_line(wifi_bitmap, items['ssid'], 40)
        _ico_line(addr_bitmap, items['ip_address'], 70)
    elif items['status'] == "hotspot":
        _ico_line(hotspot_bitmap, items['ssid'], 25)
        _ico_line(pass_bitmap, items['password'], 55)
        _ico_line(addr_bitmap, items['ip_address'], 85)
    elif items['status'] == "disconnected":
        if 'time' in items:
            _ico_line(disc_bitmap, 'Disconnected', 40)
            _ico_line(clock_bitmap, items['time'] + "s", 70)
        else:
            _ico_line(disc_bitmap, 'Disconnected', 50)
    else:
        _ico_line(nodata_bitmap, 'No data', 40)
        _add_line("Run autospot.exe", 75)
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
