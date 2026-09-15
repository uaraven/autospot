import storage
import os
import supervisor

# Set custom USB string descriptors
supervisor.set_usb_identification(
    manufacturer="uaraven",
    product="autospot"
)

storage.remount("/", readonly=False)
m = storage.getmount("/")
m.label = "AUTOSPOT"
try:
    os.remove("/status.json")
except: 
    pass
storage.remount("/", readonly=True)
storage.enable_usb_drive()

