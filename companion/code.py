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
                    elif cmd == "i":
                        status['ssid'] = payload
                    elif cmd == "p":
                        status['password'] = payload
                    elif cmd == "a":
                        status['ip_address'] = payload
                    elif cmd == "m":
                        status['message'] = payload
                    elif cmd == "t":
                        status['time'] = payload

    hal.show_info(status)
    # Small delay to keep the system responsive
    time.sleep(0.1)
