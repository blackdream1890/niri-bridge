#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
set -euo pipefail
if [[ "${NIRI_BRIDGE_CI:-}" != 1 ]]; then
    echo 'Run this script in the documented isolated build container.' >&2
    exit 1
fi
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR=/tmp/niri-bridge-target
export CARGO_PROFILE_RELEASE_DEBUG=0
export RUSTFLAGS="--remap-path-prefix=$PWD=/usr/src/niri-bridge --remap-path-prefix=${CARGO_HOME}/=/usr/src/cargo/ --remap-path-prefix=${RUSTUP_HOME}/=/usr/src/rust/"
export SOURCE_DATE_EPOCH
SOURCE_DATE_EPOCH=$(git show -s --format=%ct HEAD)
python3 -B scripts/check-public.py
actionlint
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
python3 -B -m unittest discover -s scripts -p 'test_*.py'
python3 -B ui/test_i18n.py
GDK_BACKEND=x11 xvfb-run -a -s '-screen 0 1280x1200x24' dbus-run-session -- python3 -W ignore::DeprecationWarning:gi.events -B ui/test_ui.py
NIRI_BRIDGE_UI_TEST_LANGUAGE=zh_CN GDK_BACKEND=x11 xvfb-run -a -s '-screen 0 1280x1200x24' dbus-run-session -- python3 -W ignore::DeprecationWarning:gi.events -B ui/test_ui.py
cargo test --locked --test nested_capture -- --ignored --nocapture
cargo test --locked --test encrypted_bridge -- --ignored --nocapture --test-threads=1
cargo-audit audit --deny warnings
python3 -B scripts/third_party.py --check
systemd-analyze --user verify service/niri-bridge.service
cargo build --locked --release
python3 -B scripts/release.py --binary "$CARGO_TARGET_DIR/release/niri-bridge" --output /output
runuser -u tester -- python3 -B scripts/smoke-install.py --archive /output/niri-bridge-*-ubuntu26.04-x86_64.tar.gz
# Prove the corresponding-source archive resolves its entire dependency graph
# offline using its vendor configuration and a clean Cargo cache.
source_test=$(mktemp -d /tmp/niri-bridge-source-check-XXXXXX)
tar -xzf /output/niri-bridge-*-source.tar.gz -C "$source_test"
source_root=$(find "$source_test" -mindepth 1 -maxdepth 1 -type d -print)
(
    cd "$source_root"
    CARGO_HOME="$source_test/cargo-cache" CARGO_TARGET_DIR="$source_test/target" cargo build --frozen --release
    "$source_test/target/release/niri-bridge" --version
)
echo 'Corresponding source rebuilt successfully with offline Cargo dependencies.'
