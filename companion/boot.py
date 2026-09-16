import supervisor

# Sets custom USB string descriptors and VID/PID values
# In case autospot cannot connect to the device ensure that vid and pid in autospot.toml match these values
# In case of conflict with another device, chose different PID and update it here and in autospot.toml
supervisor.set_usb_identification(
    manufacturer="autospot",
    product="companion",
    vid=0x1209,
    pid=0x3a01,
)
