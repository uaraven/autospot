# SPDX-License-Identifier: GPL-3.0-or-later
# Part of the Autospot companion; see LICENSE.

import hal
import json


maingroup = hal.initialize_display()


def read_status():
    try:
        with open("status.json", "r") as f:
            return json.load(f)
    except Exception as e:
        print("Error reading status.json:", e)
        return {"status": "unknown"}
    

status = read_status()
display = {}
if status['status'] == "connected":
    display = {
        'status': "connected",
        'ssid': status['connected']['ssid'],
        'ip_address': status['connected']['ip_address']
    }
elif status['status'] == "hotspot":
    display= {
        'status': "hotspot",
        'ssid': status['hotspot']['ssid'],
        'password': status['hotspot']['password'],
        'ip_address': status['hotspot']['ip_address']
    }
elif status['status'] == "disconnected":
    display['status'] = "disconnected"
else:
    display['status'] = "unknown"

hal.show_info(display)

while True:
    pass