# Building and testing

English · [简体中文](testing.zh-CN.md)

The repository pins Rust 1.98.1. The initial validation environment is Ubuntu 26.04 with Niri 26.04. The UI requires GTK 3, PyGObject and Cairo. Wayland client bindings use the Rust backend.

## Routine checks

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
python3 -B -m unittest discover -s scripts -p 'test_*.py'
python3 -B ui/test_i18n.py
systemd-analyze --user verify service/niri-bridge.service
```

Routine Rust tests cover protocol bounds, geometry, held-state cleanup, authentication failures, configuration revisions and rollback, certificate import checks and the private control interface. These tests do not inject input into the current desktop.

The translation test checks every marked English message against the Chinese catalog, validates placeholders and checks language-selection behavior and private preference storage.

## Desktop widget tests

These tests open a clearly labeled test window using simulated state. They do not operate the live sharing service or send physical input:

```sh
python3 -W ignore::DeprecationWarning:gi.events -B ui/test_ui.py
NIRI_BRIDGE_UI_TEST_LANGUAGE=zh_CN python3 -W ignore::DeprecationWarning:gi.events -B ui/test_ui.py
```

They exercise the actual button handlers, paired screen request construction, initial-load side effects and language switching with unsaved edits. The warning filter is limited to the installed PyGObject/Python compatibility deprecation. GTK and application errors remain visible.

The renderer draws only the application's own widgets to Cairo PNGs. It does not take a desktop screenshot or alter the shared clipboard. Inspect every page in both languages after layout changes, then remove temporary QA images.

## Isolated Niri integration

Install Weston with headless/pixman support and Niri, then run:

```sh
cargo test --locked --test nested_capture -- --ignored --nocapture
cargo test --locked --test encrypted_bridge -- --ignored --nocapture --test-threads=1
```

The fixtures create private temporary runtime directories and their own compositor sockets. Their synthetic Wayland input is restricted to those sockets. They do not import or modify the normal desktop session environment.

Coverage includes capture readiness, keys/buttons/scroll, return edges, ordinary Escape, emergency return, physical takeover, output changes, lock state and disconnect release. A native isolated session lock checks actual capture cleanup. Another test proves the virtual-keyboard shortcut limitation. Physical-key fixtures verify forwarding when Wayland delivers no keyboard events.

The desktop-control integration test sends a real local UI API request through the shared coordinator, across mutually authenticated TLS, and checks both configuration files. It verifies successful paired saves and rejects stale revisions or unavailable peer outputs without changing either configuration.

The multiple-connection suite runs two edge pairs in both directions, with the
configuration order reversed on one peer. It verifies returning through the
other connection, the actual pointer position delivered to an isolated observer,
and released keyboard state after focus returns. The UI suite verifies adding,
selecting and removing pairs, overlap rejection and draft preservation across
language changes. Configuration tests cover migration from `[edge]`, comment
preservation, connection ordering and transaction-lock release. These checks do
not replace multi-monitor physical acceptance.

The fixtures' keyboard injection and physical activity/lock markers are controlled test backends. They do not by themselves prove real uinput, logind signals or all touchpad gestures. Each fixture cleans up its own processes and temporary files.

## Read-only diagnosis

```sh
niri-bridge doctor --json
niri-bridge check-config --config ~/.config/niri-bridge/config.toml
niri-bridge check-session
```

`doctor` distinguishes advertised interfaces, permissions and unperformed behavior tests. It excludes private keys, pairing addresses, device serials and input contents.

## Explicit real-device probes

Agree on timing before these probes: they affect the selected desktop briefly.

```sh
niri-bridge capture-test --output eDP-1 --edge top --start 0.35 --end 0.65 --seconds 15
niri-bridge verify-input --config ~/.config/niri-bridge/config.toml --overview-shortcut Super+Shift+O
```

`capture-test` shows a narrow blue edge. Enter it to test capture; Escape or the bounded timeout restores control. It reports counts only. `verify-input` tests the receiving keyboard/pointer backend inside the program's own capture surface, checks the supplied overview shortcut, restores state and removes its temporary keyboard. Concurrent physical input or input-method handoff can interrupt this probe; interpret its actual booleans rather than counting a process exit as input success.

The native touchpad example reads a non-identifying capability profile from the source and tests the destination's native gesture interpretation:

```sh
cargo run --locked --release --example verify_touchpad -- profile --config ~/.config/niri-bridge/config.toml --output /tmp/touchpad-profile.json
cargo run --locked --release --example verify_touchpad -- verify --profile /tmp/touchpad-profile.json
```

Use the source computer's profile on the destination. The probe creates a virtual touchpad, tests three-finger workspace switching and four-finger overview, restores the focused window and overview state, and removes the device. It does not record the user's gesture data.

## Real-device acceptance

The initial two computers have user-confirmed bidirectional keyboard/pointer control, native three/four-finger gesture forwarding and restored touchpad responsiveness after the event-timing correction. Both user services were enabled and restarted successfully. Physical takeover was also observed in the real service logs.

The 0.2 desktop interface has separate widget, language and paired-file transaction tests. Both installed desktops passed actual tray registration, menu loading, opening, pause/start and quit checks. Configuration and startup state were preserved, and locked sessions correctly disabled screen saves. With both sessions unlocked, real screen-entry saves initiated on either installed desktop updated both files successfully. The original configurations were restored byte-for-byte and both sides reconnected. Keep this evidence separate from simulated UI screenshots. Long suspend cycles, all Chinese editing scenarios, extended mouse buttons and every combined gesture still need broader acceptance.

## Isolated Linux kernel and libinput handoff

The kernel regression runs in QEMU with no host input devices, display, network,
monitor or shared filesystem. Both the Rust fixture and C observer refuse to run
without the dedicated guest boot marker. Install `qemu-system-x86`,
`busybox-static`, `libinput-dev` and `libudev-dev`; provide a readable Ubuntu kernel
image with built-in uinput support. The CI image extracts its pinned test kernel
from the signed Ubuntu package repository without installing it as a host kernel.

```sh
python3 -B scripts/test-kernel-input.py --kernel /path/to/test-vmlinuz
```

The test checks all 250 initial/captured/next-slot and held-contact combinations,
then reproduces a legacy ungrab followed by a stroke that leaves a ghost contact.
A real libinput observer verifies repeated pointer motion, three/four-finger
swipes and absence of synthetic button, motion or gesture starts during handoff,
with tapping enabled. Ordinary handoffs must generate no input-state errors.
Legacy recovery may diagnose duplicate endings once; subsequent gestures must
work without further errors. The test never opens the running desktop's devices.

Python lifecycle tests simulate service and socket state, covering idle first
launch, stop-before-exit ordering, legacy and abnormal-exit recovery, refusal to
manage an independent process, private upgrade backups, startup migration and
preservation of customized launchers and files. UI tests separately exercise
Start/Stop, tray quit failure and closing the window to the tray.

## 0.2.0-beta.3 physical acceptance

Both installed Ubuntu/Niri desktops were upgraded with the shared installer.
Configuration hashes and identity-file metadata were unchanged. Repeated Start/Stop
operations used the real tray callbacks. Closing each application window retained
the tray and sharing; quitting from each tray stopped the backend and left zero
NiriBridge virtual input devices. Reopening each interface kept sharing stopped,
then explicit Start sharing reconnected both configured screen connections.

The user subsequently confirmed one-finger pointer movement, three-finger workspace
switching, four-finger overview, crossings through both physical connections and
normal local touchpad operation after Stop sharing. This is acceptance on the
initial hardware, separate from VM and simulated-widget evidence. Long suspend,
more device models, extended buttons and combined gestures remain broader tests.
