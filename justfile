# Task runner for the streamdock workspace. `just --list` shows all recipes.

default: build

# Build everything
build:
    cargo build --workspace --all-targets

# Run all tests (Rust and web)
test:
    cargo test --workspace
    cd web && pnpm test

# Format check, clippy (warnings are errors, complexity limits in clippy.toml), proto lint and the
# web lint (ESLint with complexity limits) and typecheck. The pre-commit hook runs exactly this.
lint:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cd proto && buf lint
    cd web && pnpm lint && pnpm typecheck

# Everything a CI run would check (H15-36): lint, then all tests
check: lint test

# Integration smoke test: release daemon with the simulated dock only, checks UI and API (H15-59)
smoke:
    cargo build --release -p streamdockd
    scripts/smoke.sh target/release/streamdockd

# Run `just lint` before every commit in this repo and all its worktrees (.githooks/pre-commit)
install-hooks:
    git config core.hooksPath .githooks
    @echo "pre-commit hook enabled: just lint runs before every commit (skip once with git commit --no-verify)"

# Format the code
fmt:
    cargo fmt --all

# Run the daemon (UI on http://127.0.0.1:7315)
run *ARGS:
    cargo run -p streamdockd -- {{ARGS}}

# Needs `zig`, `cargo install cargo-zigbuild` and `rustup target add armv7-unknown-linux-musleabihf`.
# Static Cortex-A7/NEON build (Luckfox Pico / RV1106), H15-34/H15-67
build-armv7:
    cargo zigbuild --profile release-armv7 -p streamdockd --target armv7-unknown-linux-musleabihf
    @ls -la "${CARGO_TARGET_DIR:-target}/armv7-unknown-linux-musleabihf/release-armv7/streamdockd"

# A safe dev instance on http://127.0.0.1:7397: simulated dock, demo modes only (packaging/dev-config.toml)
dev:
    STREAMDOCKD_CONFIG=packaging/dev-config.toml cargo run -p streamdockd

# Demo on http://127.0.0.1:7399: the simulated dock against a fake Home Assistant and Lyrion with
# neutral names (examples/demo_backend.rs); for screenshots and UI work, never a real device (H15-102)
demo:
    #!/bin/sh
    set -eu
    cargo build -q -p streamdockd --example demo_backend
    target/debug/examples/demo_backend & backend=$!
    trap 'kill $backend' EXIT INT TERM
    dir=$(mktemp -d); cp packaging/demo-config.toml "$dir/config.toml"; echo demo > "$dir/homeassistant.token"
    STREAMDOCKD_CONFIG="$dir/config.toml" XDG_STATE_HOME="$dir/state" STREAMDOCKD_PLAYER_SILENT=1 cargo run -p streamdockd

# Regenerate the TypeScript API from proto/ (after changing device.proto)
gen:
    cd web && pnpm gen

# Follow the production daemon's log on this machine
logs:
    journalctl --user -u streamdockd -f

# Every streamdockd on the LAN (mDNS _streamdockd._tcp): name, host, address, port, TXT
discover:
    @avahi-browse -rpt _streamdockd._tcp | awk -F';' '$1=="=" && $3=="IPv4" {print $4, $7, $8, $9, $10}' | sort -u

# Without a target it picks the first ARM streamdockd that mDNS finds (the first install needs
# root@<ip>). Extra ssh options via STREAMDOCK_SSH_OPTS, e.g.
# "-i ~/.ssh/luckfox -o IdentitiesOnly=yes".
# Build the ARMv7 binary and install/update it on a Luckfox Pico (Ubuntu) over SSH
deploy-luckfox target="":
    #!/bin/sh
    set -eu
    target="{{target}}"
    if [ -z "$target" ]; then
        ip=$(avahi-browse -rpt _streamdockd._tcp | awk -F';' '$1=="=" && $3=="IPv4" && $10 ~ /arch=arm/ {print $8; exit}')
        [ -n "$ip" ] || { echo "no ARM streamdockd found via mDNS; run: just deploy-luckfox root@<ip>"; exit 1; }
        target=root@$ip
        echo "found $target via mDNS"
    fi
    packaging/luckfox/install.sh "$target" ${STREAMDOCK_SSH_OPTS:-}

# The branch must already be rebased on main. Tests it, fast-forwards main in the checkout that has
# main, pushes main to origin, then deploys production (`just deploy`). Never forces (H15-59).
# Integrate a branch (default: the current one) into main from any worktree: test, merge, push, deploy
integrate branch="":
    #!/bin/sh
    set -eu
    branch="{{branch}}"
    [ -n "$branch" ] || branch=$(git symbolic-ref --short HEAD)
    [ "$branch" != main ] || { echo "integrate a feature branch, not main"; exit 1; }
    main_dir=$(git worktree list --porcelain | awk '/^worktree /{w=substr($0,10)} $0=="branch refs/heads/main"{print w}')
    [ -n "$main_dir" ] || { echo "no worktree has main checked out"; exit 1; }
    git merge-base --is-ancestor main "$branch" || { echo "$branch is not rebased on main: git rebase main"; exit 1; }
    # The pre-commit hook checked the tree before the rebase; test what lands on main.
    if [ "$(git rev-parse HEAD)" = "$(git rev-parse "$branch")" ]; then
        cargo build --workspace --all-targets
        cargo test --workspace
    else
        echo "note: $branch is not checked out here, skipping the test run"
    fi
    git -C "$main_dir" merge --ff-only "$branch"
    # A failed push (no key, no network) must not block the deploy (H15-59), but it must not
    # go unnoticed either: deploy, then fail loudly at the end so the caller sees it (H15-59).
    pushed=yes
    git -C "$main_dir" push origin main || pushed=no
    (cd "$main_dir" && just deploy)
    if [ "$pushed" = no ]; then
        echo >&2 ""
        echo >&2 "ERROR: main is merged and deployed, but NOT pushed (origin/main is behind)."
        echo >&2 "       Push it now: git -C $main_dir push origin main"
        exit 1
    fi

# Push the current branch to origin (branches that outlive a session)
push-branch:
    git push -u origin "$(git symbolic-ref --short HEAD)"

# Install the udev rule so the dock works without root
udev-install:
    sudo install -m644 udev/70-streamdock.rules /etc/udev/rules.d/
    sudo udevadm control --reload
    sudo udevadm trigger

# Run it from the main checkout after every merge, so :7315 always runs the latest main.
# Production on this machine: release build as the streamdockd user service, restarted
deploy:
    cargo build --release -p streamdockd
    install -Dm755 target/release/streamdockd ~/.local/bin/streamdockd.new
    mv ~/.local/bin/streamdockd.new ~/.local/bin/streamdockd
    install -Dm644 packaging/streamdockd.service ~/.config/systemd/user/streamdockd.service
    systemctl --user daemon-reload
    systemctl --user enable streamdockd.service
    systemctl --user restart streamdockd.service
    @sleep 2 && systemctl --user --no-pager --lines=3 status streamdockd.service | head -12

# Install the release daemon as a systemd user service (~/.local/bin + ~/.config/systemd/user)
install-service:
    cargo build --release -p streamdockd
    install -Dm755 target/release/streamdockd ~/.local/bin/streamdockd
    install -Dm644 packaging/streamdockd.service ~/.config/systemd/user/streamdockd.service
    test -e ~/.config/streamdockd/config.toml || install -Dm644 packaging/config.example.toml ~/.config/streamdockd/config.toml
    systemctl --user daemon-reload
    systemctl --user enable --now streamdockd.service
