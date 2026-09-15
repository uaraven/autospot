# SPDX-License-Identifier: GPL-3.0-or-later
# Part of the Autospot companion; see LICENSE.

import hal
import sys
import json
import supervisor
import time

    
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

input_buffer = ""
while True:
    if supervisor.runtime.serial_bytes_available:
            # Read available characters from standard input
            raw_data = sys.stdin.read(supervisor.runtime.serial_bytes_available)
            input_buffer += raw_data

            # Process complete lines terminated by a newline character (\n)
            while "\n" in input_buffer:
                line, input_buffer = input_buffer.split("\n", 1)
                line = line.strip()  # Remove \r or trailing whitespace
                
                if line:
                    # --- PROCESS YOUR DATA HERE ---
                    print(f"Received Command: '{line}'")
                    
                    # Example action based on received string
                    if line == "LED_ON":
                        display['status'] = "disconnected"
                        hal.show_info(display)
                        pass

    # Small delay to keep the system responsive
    time.sleep(0.1)