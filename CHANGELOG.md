# StreamDock updates

## 2026-10-21

- `just demo` runs the dashboard against a fake Home Assistant and Lyrion with neutral demo data, so you can try every screen without a real house; the README screenshots now come from it. H15-102
- A README with screenshots of every screen and settings page: what the dock does, how to install it on a PC or a Luckfox, and how to configure it. H15-100
- Ready for an open-source release: MIT OR Apache-2.0 licence files, CONTRIBUTING and a list of the bundled third-party material. A music tab without any player now controls the first player Lyrion lists. The commit link in the header follows the repository the binary was built from. H15-101

## 2026-10-19

- On the Luckfox, the dock now works plugged straight into the USB-A port, without a hub: a boot service raises the USB signal level the dock needs (`usb-phy-tune.service`). H15-98
- `streamdockd --version` and `--help` print and exit; any other argument is refused before anything starts, so a stray command can't start a second daemon next to the service. H15-99
- Light tabs can show the brightness and colour temperature as bars on keys (`level_keys`): the brightness as a filled bar with %, the colour temperature from warm to cold with Kelvin, and the light's own key gets a brightness strip. They follow a knob turn at once. Turning the dimmer while the lights are off switches them on from 1 %; turning it down to 0 switches them off. H15-97
- On every music tab the big knob is now the volume, with the level shown as a filled bar and percentage on key 3 for a few seconds after each change (or all the time); knob 1 sets the bass on the music screen and pages through the playlists. In headset mode the music screen controls the headset player, up to its volume limit. H15-95
- Lights and blinds now use the built-in icons, with light on/off and blind movement shown by their state. A pinned key can override its icon with a built-in name or an uploaded image. H15-96
- After a restart or update, the dashboard no longer shows old key images from before: each image is identified by its content, so the browser only reuses its cache when the image is really the same. H15-94
- Give a blind separate up, stop and down keys, with position and movement shown live. Press a direction again while it moves to stop. The key editor also offers a service targeted only at the chosen entity. H15-93
- A settings screen on the dock: it lists the paired Bluetooth headphones and speakers with their state and battery, and a press connects or disconnects one; with more than four, page with the last key or knob 1. Turn it on under Settings → Integrations → Settings screen. H15-91
- Headset mode on the dock's settings screen: while a headset is connected, a key moves the music (queue and position) from the speaker to the built-in player and the headset, and back. `[player] max_volume` caps the built-in player's volume, e.g. for a child's headphones. H15-91
- Music tabs can show playback controls instead of playlists: the now-playing band with big ⏮ ⏯ ⏭ keys below it, knob 1 for volume (press to play or pause) and knob 2 for the bass of a Sony speaker, set on the speaker itself. A playlist tab can use all six keys and switch to the music screen after a press. H15-90
- See what's playing on the dock: the music tab shows the current track as a scrolling band across the top three keys (with the cover behind it and a progress bar), and the dashboard shows it live. Choose the keys, the text, scrolling or cut, speed, cover and progress per music tab (tab menu → Now playing…); pressing the band plays or pauses. H15-88
- Choose which Lyrion player a music tab controls: every player the server announces, by name with its state, from Settings → Integrations or the tab's menu (Player…). The choice is saved by the player's id, so renaming it in Lyrion doesn't matter; new setups default to the first player the server lists. A switched-off or forgotten player shows as such on the keys instead of "Lyrion offline". H15-87

## 2026-10-18

- Cleaner resampling in the built-in player: 44.1 kHz music is converted to the output's 48 kHz before the sound processing, and changes between tracks at different sample rates are gapless. The quality is selectable under Settings → Lyrion player; "fast" uses about the CPU of the old behaviour, "balanced" (default) a little more. H15-80
- Music playback uses about 32% less audio-chain CPU and 23 MiB less memory on the Luckfox. Sound processing now runs inside the player, with the same tone, correction EQ, crossfeed, balance and limiter controls. Idle network traffic is also much lighter. H15-67
- See the running build’s revision and local build time in the header, open its source commit, and read these updates from the menu. H15-77
- Play music directly from Lyrion with the built-in player, including replay gain, gapless playback and live player settings. H15-47 H15-55 H15-61
- Shape the sound with character, correction presets, loudness, crossfeed and a protective limiter. H15-50 H15-54
- Choose a Bluetooth codec and LDAC quality for each headset. LDAC remains the default. H15-78
- Set an output priority list so your preferred headset takes over when it is available, with a fallback when it is absent. H15-79
- Start Home Assistant Assist from a key, or use tap-to-talk with automatic end-of-speech detection. Wake-word settings and model uploads support hands-free activation when a microphone is available. H15-43 H15-68 H15-69
- Configure Ethernet, Wi-Fi, Bluetooth and audio from the settings page; IPv6 changes preserve your IPv4 configuration. H15-38 H15-42 H15-63
- Control the dock’s keys and knobs from the web dashboard, and customize tabs, actions, key images and icons. H15-28 H15-29 H15-32 H15-48 H15-51
- Run StreamDock on the Luckfox board and discover it automatically on your local network. H15-34 H15-35
- Home Assistant setup uses much less memory, keeping the Luckfox daemon around 20 MiB before music playback. H15-57
