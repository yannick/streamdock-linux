-- WirePlumber on a headless board (H15-38): the audio session runs as the lingering user "sound",
-- without a login seat and with nobody else to reserve ALSA devices from.
alsa_monitor.properties["alsa.reserve"] = false
