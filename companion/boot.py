import supervisor

# Set custom USB string descriptors
# In case autospot cannot connect to the device
# uncomment VID and PID settings and
# configure vid and pid in autospot.toml
supervisor.set_usb_identification(
    manufacturer="autospot",
    product="companion",
    vid=0x1209,
    pid=0x3a01,
)
