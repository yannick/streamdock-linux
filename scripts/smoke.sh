#!/bin/sh
# Integration smoke test (H15-59): starts a streamdockd binary with the simulated dock only
# (packaging/dev-config.toml: hardware = false, demo modes, loopback), then checks that the UI,
# the design page and the Connect API answer. Never touches the real dock or real integrations.
# Usage: scripts/smoke.sh [binary]   (default target/release/streamdockd)
set -eu
bin=${1:-target/release/streamdockd}
addr=127.0.0.1:${SMOKE_PORT:-7398}
state=$(mktemp -d)
log="$state/streamdockd.log"
HOME="$state" XDG_CONFIG_HOME="$state" XDG_STATE_HOME="$state" \
  STREAMDOCKD_CONFIG=packaging/dev-config.toml STREAMDOCKD_ADDR="$addr" STREAMDOCKD_HARDWARE=0 \
  "$bin" >"$log" 2>&1 &
pid=$!
trap 'kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; rm -rf "$state"' EXIT

up=""
for _ in $(seq 1 100); do
  if curl -fsS -o /dev/null "http://$addr/"; then up=1; break; fi
  kill -0 "$pid" 2>/dev/null || break
  sleep 0.1
done
[ -n "$up" ] || { echo "streamdockd did not come up on $addr"; cat "$log"; exit 1; }

fail=0
check() { # path, expected text
  body=$(curl -fsS "http://$addr$1") || { echo "FAIL $1: request failed"; fail=1; return; }
  case "$body" in *"$2"*) echo "ok   $1" ;; *) echo "FAIL $1: no \"$2\""; fail=1 ;; esac
}
check / "<html"
check /design "<html"
check /keys "<html"
check /changelog "Changelog"
# Connect unary call, JSON codec: the simulated dock FAKE-0001 is listed.
devices=$(curl -fsS -X POST -H 'Content-Type: application/json' -d '{}' \
  "http://$addr/streamdock.v1.DeviceService/GetDevices") || devices=""
case "$devices" in *FAKE-0001*) echo "ok   GetDevices (FAKE-0001)" ;; *) echo "FAIL GetDevices: $devices"; fail=1 ;; esac
version=$(curl -fsS -X POST -H 'Content-Type: application/json' -d '{}' \
  "http://$addr/streamdock.v1.DeviceService/GetVersion") || version=""
case "$version" in *revision*commit*builtAt*) echo "ok   GetVersion" ;; *) echo "FAIL GetVersion: $version"; fail=1 ;; esac
kill -0 "$pid" 2>/dev/null || { echo "FAIL streamdockd exited"; fail=1; }
[ "$fail" = 0 ] || { cat "$log"; exit 1; }
echo "smoke test passed"
