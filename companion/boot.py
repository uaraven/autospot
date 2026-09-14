import storage
import os

storage.remount("/", readonly=False)
m = storage.getmount("/")
m.label = "AUTOSPOT"
try:
    os.remove("/status.json")
except: 
    pass
storage.remount("/", readonly=True)
storage.enable_usb_drive()

