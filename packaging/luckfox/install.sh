#!/bin/sh
# Installs or updates streamdockd on a Luckfox Pico (Ultra) running Ubuntu, over SSH (H15-34).
#   packaging/luckfox/install.sh <ssh destination> [extra ssh options …]
#   e.g. packaging/luckfox/install.sh root@192.168.1.141
# Builds the static ARMv7 binary first (just build-armv7). The board needs a kernel with hidraw
# (the H15-34 image). A config is only written if the board has none yet.
set -eu
dest=$1; shift
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
bin=$root/target/armv7-unknown-linux-musleabihf/release-armv7/streamdockd

(cd "$root" && just build-armv7)
scp "$@" "$bin" "$dest:/tmp/streamdockd.new"
scp "$@" "$here/streamdockd.service" "$dest:/tmp/streamdockd.service"
scp "$@" "$here/70-streamdock.rules" "$dest:/tmp/70-streamdock.rules"
scp "$@" "$here/50-streamdockd-network.pkla" "$here/streamdockd-bluez.conf" "$dest:/tmp/"
scp "$@" "$root/packaging/config.example.toml" "$dest:/tmp/streamdockd.config.example.toml"
scp "$@" "$here/bt-attach.service" "$dest:/tmp/bt-attach.service"
scp "$@" "$here/usb-phy-tune.py" "$here/usb-phy-tune.service" "$dest:/tmp/"
scp "$@" -r "$here/audio" "$dest:/tmp/streamdockd-audio"
ssh "$@" "$dest" 'set -eu
install -m755 /tmp/streamdockd.new /usr/local/bin/streamdockd
install -m644 /tmp/streamdockd.service /etc/systemd/system/streamdockd.service
install -m644 /tmp/70-streamdock.rules /etc/udev/rules.d/70-streamdock.rules
# Wi-Fi and Bluetooth from the settings page (H15-38): the groups the service runs in must exist.
getent group netdev >/dev/null || groupadd -r netdev
getent group bluetooth >/dev/null || groupadd -r bluetooth
getent group plugdev >/dev/null || groupadd -r plugdev
grep -q "^streamdockd:" /etc/passwd || useradd --system --user-group --home-dir /var/lib/streamdockd --no-create-home --shell /usr/sbin/nologin streamdockd
usermod -a -G plugdev,netdev,bluetooth streamdockd
install -d -m755 /etc/polkit-1/localauthority/50-local.d
install -m644 /tmp/50-streamdockd-network.pkla /etc/polkit-1/localauthority/50-local.d/50-streamdockd-network.pkla
install -m644 /tmp/streamdockd-bluez.conf /etc/dbus-1/system.d/streamdockd-bluez.conf
systemctl reload dbus 2>/dev/null || true
install -d -m755 /usr/local/share/streamdockd
install -m644 /tmp/streamdockd.config.example.toml /usr/local/share/streamdockd/config.example.toml
rm -f /tmp/streamdockd.new /tmp/streamdockd.service /tmp/70-streamdock.rules /tmp/streamdockd.config.example.toml
rm -f /tmp/50-streamdockd-network.pkla /tmp/streamdockd-bluez.conf
# Bluetooth (H15-38): controller attach at boot, plus BlueZ and PipeWire (LDAC) for audio.
install -m644 /tmp/bt-attach.service /etc/systemd/system/bt-attach.service && rm -f /tmp/bt-attach.service
# DSP runs in the player (H15-67); no LADSPA packages are required.
if ! dpkg -s bluez libspa-0.2-bluetooth wireplumber >/dev/null 2>&1; then
  DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
    bluez pipewire pipewire-pulse pipewire-audio-client-libraries wireplumber libspa-0.2-bluetooth pulseaudio-utils
fi
systemctl daemon-reload && systemctl enable --now bt-attach.service
# The dock directly on USB-A, without a hub (H15-98): higher USB PHY eye height at boot.
install -d -m755 /usr/local/lib/streamdockd
install -m755 /tmp/usb-phy-tune.py /usr/local/lib/streamdockd/usb-phy-tune.py
install -m644 /tmp/usb-phy-tune.service /etc/systemd/system/usb-phy-tune.service
rm -f /tmp/usb-phy-tune.py /tmp/usb-phy-tune.service
systemctl daemon-reload && systemctl enable usb-phy-tune.service && systemctl restart usb-phy-tune.service
# Battery levels of headsets (H15-54): BlueZ offers Battery1 and the battery provider PipeWire uses
# only with Experimental. Restarts BlueZ once, when the setting is new.
if ! grep -q "^Experimental = true" /etc/bluetooth/main.conf; then
  sed -i "/^\[General\]/a Experimental = true" /etc/bluetooth/main.conf
  systemctl restart bluetooth
fi
# Audio (H15-38): PipeWire + WirePlumber do not run as root; the lingering user "sound" runs them
# (and squeezelite) without a login. Its session bus comes from dbus-user-session.
dpkg -s dbus-user-session >/dev/null 2>&1 || DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends dbus-user-session
id sound >/dev/null 2>&1 || useradd --create-home --groups audio,bluetooth --shell /usr/sbin/nologin sound
loginctl disable-linger root 2>/dev/null || true
wp=/home/sound/.config/wireplumber
# The audio stack restarts only when its files change (H15-54): a deploy must not cut the music.
audio_files="$wp/main.lua.d/51-headless.lua $wp/bluetooth.lua.d/30-bluez-monitor.lua
  /home/sound/.config/systemd/user/squeezelite.service /home/sound/.config/pipewire/streamdock-eq.conf
  /home/sound/.config/systemd/user/streamdock-eq.service /home/sound/.config/systemd/user/pipewire.service.d/streamdockd.conf
  /etc/security/limits.d/95-sound-realtime.conf"
audio_before=$(cat $audio_files 2>/dev/null | md5sum)
install -d $wp/main.lua.d $wp/bluetooth.lua.d /home/sound/.config/systemd/user/default.target.wants
install -m644 /tmp/streamdockd-audio/51-headless.lua $wp/main.lua.d/51-headless.lua
# The Bluetooth monitor waits for an active login seat, which a headless user never has.
grep -v "load_optional_module(\"logind\")" /usr/share/wireplumber/bluetooth.lua.d/30-bluez-monitor.lua > $wp/bluetooth.lua.d/30-bluez-monitor.lua
# The A2DP rate pin (H15-78) equals the PipeWire default, so it is not in audio_files: it needs no
# restart and takes effect with the next one.
install -m644 /tmp/streamdockd-audio/51-streamdockd-bluez.lua $wp/bluetooth.lua.d/51-streamdockd-bluez.lua
install -m644 /tmp/streamdockd-audio/squeezelite.service /home/sound/.config/systemd/user/squeezelite.service
# squeezelite plays only while the built-in player (H15-47, [player] enabled = true) is off: two
# players on one board would both show up in Lyrion and fight over the output.
if sed -n "/^\[player\]/,/^\[/p" /var/lib/streamdockd/config.toml 2>/dev/null | grep -q "^enabled *= *true"; then
  rm -f /home/sound/.config/systemd/user/default.target.wants/squeezelite.service
  runuser -u sound -- env XDG_RUNTIME_DIR=/run/user/$(id -u sound) systemctl --user stop squeezelite 2>/dev/null || true
else
  ln -sf ../squeezelite.service /home/sound/.config/systemd/user/default.target.wants/squeezelite.service
fi
# H15-67: remove the old five-stage DSP. Keeping its paths in audio_files above makes
# this migration restart sound once; subsequent identical deploys leave the stack running.
runuser -u sound -- env XDG_RUNTIME_DIR=/run/user/$(id -u sound) systemctl --user disable --now streamdock-eq.service 2>/dev/null || true
rm -f /home/sound/.config/systemd/user/default.target.wants/streamdock-eq.service \
  /home/sound/.config/systemd/user/streamdock-eq.service /home/sound/.config/pipewire/streamdock-eq.conf
# streamdockd reaches the physical PipeWire outputs through the group sound.
install -d /home/sound/.config/pipewire /home/sound/.config/systemd/user/pipewire.service.d
install -m644 /tmp/streamdockd-audio/pipewire-streamdockd.conf /home/sound/.config/systemd/user/pipewire.service.d/streamdockd.conf
chown -R sound:sound /home/sound/.config
install -m644 /tmp/streamdockd-audio/pw-cat.conf /usr/local/share/streamdockd/pw-cat.conf
rm -rf /tmp/streamdockd-audio
# The own squeezelite of the image (SysV) grabs the codec directly: off for good.
/etc/init.d/squeezelite stop >/dev/null 2>&1 || true
pkill -x -u root squeezelite || true
update-rc.d squeezelite disable >/dev/null 2>&1 || true
systemctl mask squeezelite >/dev/null 2>&1 || true
# Realtime scheduling for the audio user (H15-50): without RTKit, PipeWire and squeezelite only get
# their realtime threads through these limits (pam_limits applies them to the user manager). Not
# memlock: squeezelite would lock its whole address space (~150 MB of 234).
printf "%s\n" "# streamdockd install.sh (H15-50): realtime audio threads for the user sound." \
  "sound - rtprio 95" "sound - nice -15" > /etc/security/limits.d/95-sound-realtime.conf
loginctl enable-linger sound
if [ "$(cat $audio_files 2>/dev/null | md5sum)" != "$audio_before" ] || ! systemctl is-active -q user@$(id -u sound); then
  echo "audio configuration changed: restarting the audio of the user sound"
  systemctl restart user@$(id -u sound)
fi
udevadm control --reload && udevadm trigger --subsystem-match=hidraw --subsystem-match=usb || true
systemctl daemon-reload
# The first start creates the state directory, owned by the streamdockd user (a former DynamicUser
# directory in /var/lib/private is moved back by systemd).
systemctl enable --now streamdockd
state=/var/lib/streamdockd
for i in 1 2 3 4 5 6 7 8 9 10; do [ -d $state ] && break; sleep 0.5; done
if [ ! -e $state/config.toml ]; then
  printf "%s\n" \
    "# streamdockd on this board; edit on the settings page or here (see /usr/local/share/streamdockd/config.example.toml)." \
    "listen = \"0.0.0.0:7315\"" "also_listen = [\"0.0.0.0:80\"]" > $state/config.toml
  chown --reference=$state $state/config.toml
fi
# Wi-Fi and Bluetooth of this board are managed from the settings page (H15-38).
if ! grep -q "^\[network\]" $state/config.toml; then
  printf "\n%s\n%s\n" "[network]" "manage = true" >> $state/config.toml
fi
# So is its audio (H15-50).
if ! grep -q "^\[audio\]" $state/config.toml; then
  printf "\n%s\n%s\n" "[audio]" "manage = true" >> $state/config.toml
fi
systemctl restart streamdockd
sleep 1
systemctl --no-pager --lines=5 status streamdockd || true'
