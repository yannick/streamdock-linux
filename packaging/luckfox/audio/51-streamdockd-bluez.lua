-- streamdockd (H15-78): when PipeWire configures an A2DP link it prefers this rate, the audio
-- graph's. 48 kHz is PipeWire 0.3.48's built-in default too; pinned so an update can't change it.
-- A headset that configures the link itself picks its own rate: streamdockd renegotiates that.
bluez_monitor.properties["bluez5.default.rate"] = 48000
