# SPDX-License-Identifier: GPL-3.0-or-later
# Part of the Autospot companion; see LICENSE.

import hal
import sys
import json
import supervisor
import time


maingroup = hal.initialize_display()

status = {
    'status': "no-data"
}


input_buffer = ""
updated = True
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
                    separator = line.find(":")
                    if separator == -1:
                        continue
                    cmd = line[:separator]
                    payload = line[separator+1:]
                    # Example action based on received string
                    if cmd == "s":
                        status['status'] = payload
                        updated = True
                    elif cmd == "i":
                        status['ssid'] = payload
                        updated = True
                    elif cmd == "p":
                        status['password'] = payload
                        updated = True
                    elif cmd == "a":
                        status['ip_address'] = payload
                        updated = True
                    elif cmd == "m":
                        status['message'] = payload
                        updated = True

    if updated:
        hal.show_info(status)
        updated = False
    # Small delay to keep the system responsive
    time.sleep(0.1)
