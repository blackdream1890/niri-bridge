# Design and validation rationale

English · [简体中文](design.zh-CN.md)

## Components

The Rust service owns input capture, authenticated transport, destination injection, recovery and configuration synchronization. The Python/GTK desktop interface owns user interaction and communicates through a private Unix control socket. Opening the interface leaves sharing stopped until Start sharing is selected. Window close hides to the tray, while tray quit waits for the managed backend to stop. Changing the interface language retains the sharing state.

| Input | Source | Destination |
| --- | --- | --- |
| Keyboard | Selected physical evdev events; a Wayland surface owns focus and inhibits local shortcuts | uinput keyboard, processed by the destination Niri session and input method |
| Mouse | Layer-shell edge, relative pointer, pointer lock and shortcut inhibition | wlr-virtual-pointer with output-aware placement |
| Native touchpad | Validated slot-based contact frames routed to the selected computer | A local or remote uinput touchpad, interpreted natively by Niri/libinput |

The TCP client/listener choice is independent of input direction. Either computer can provide the physical input. Keyboard and touchpad gestures follow the pointer's destination. Physical input on the receiving computer ends sharing; either lock or unavailable session pauses it.

## Why these paths are used

Protocol advertisement was checked on both initial machines. Portal advertisements were not treated as proof of working portal sessions, and the implementation does not depend on an unverified Mutter-backed capture session.

The [Smithay virtual keyboard implementation](https://github.com/Smithay/smithay/blob/ff5fa7df392cecfba049ffed55cdaa4e98a8e7ef/src/wayland/virtual_keyboard/virtual_keyboard_handle.rs) sends keys to the focused client. An isolated test demonstrated that it did not trigger the tested Niri global binding. The receiving keyboard therefore uses uinput.

Actual Fcitx5 sessions also exposed key loss during source-side Wayland focus transfer. The old input-method grab is deactivated asynchronously by the [input-method implementation](https://github.com/Smithay/smithay/blob/ff5fa7df392cecfba049ffed55cdaa4e98a8e7ef/src/wayland/input_method/input_method_handle.rs). The source reads selected physical keyboard events directly so input-method filtering cannot remove keys intended for the other computer. The program does not disable or reconfigure the input method.

Niri handles three- and four-finger desktop gestures before delivering client pointer gestures. Pointer lock alone therefore leaves those desktop gestures active on the source. Native contact frames are routed before Niri interprets them, allowing the destination to retain native gesture behavior and animation.

## Timing, ownership and release

Native touchpad readiness uses asynchronous file-descriptor notifications. Original frames use `CLOCK_MONOTONIC`; local replay retains their timestamps, while remote replay maps them to the destination clock and preserves frame intervals. This avoids both an extra fixed polling delay and acceleration changes caused by replaying queued frames with collapsed timestamps.

Initial touchpad ownership waits for all fingers and buttons to be released so the original compositor reader is neutral. Current contacts are synchronized when the destination changes. Ending a session releases virtual contacts and buttons. Physical handoff privately clears contacts and changes bounded axis values while grabbed, then announces neutral axes after ungrabbing. This updates the original reader's slot selection even when the kernel would otherwise filter an unchanged `ABS_MT_SLOT`. Only locally generated neutral handoff data is written to a selected physical touchpad; network payloads are never written there. Selected physical keyboards remain read-only and are not exclusively grabbed.

An older backend can already have left a ghost contact in a different compositor slot. The explicit `restore-input` helper, used by upgrade and abnormal-stop recovery, waits for physical idle and gains exclusive ownership before forcing neutral endings for every contact slot. Contact starts stay private under that grab. This legacy recovery may produce one-time libevdev duplicate-ending diagnostics for slots that were already idle; ordinary fixed-backend handoff does not require those extra endings. The VM test reproduces the old stale-slot failure and verifies subsequent native pointer and gesture recognition.

The installer and package updater share the same stop-and-recover path with the UI model. A legacy interface without a quit action must be saved and closed before application files are replaced. Updates keep private automatic backups, preserve pairing identities and leave sharing stopped. Login startup opens the interface through its own user service. Package hooks run bundled migration code as each desktop user; they never execute user-controlled application code as root.

The source begins remote control only after pointer lock, keyboard focus and shortcut inhibition are ready. Held keyboard states are synchronized on entry. Keys held on more than one selected keyboard are aggregated so the first device's release does not release another device's still-held key.

Input queues, writes and reads are bounded. Disconnection, lock, takeover, output changes and exit clean up held states and capture. The emergency chord, Ctrl+Alt+Shift+Escape, is recognized from source input without waiting for a network reply. Ordinary Escape is forwarded.

Absolute pointer placement applies the inverse of the destination output transform. Wayland scroll-source metadata is emitted after axis data in the same frame to preserve Niri's Finger source interpretation.

## Desktop management

The user control directory is private, the socket has mode 0600, and peer credentials must identify the same user. An instance lock prevents duplicate service ownership. The API reports actual authenticated connection state, control direction, lock availability, desktop metadata and measured round-trip time; it does not expose input contents.

Display metadata is refreshed outside the input loop and sent over the paired encrypted connection. The interface keeps drafts separate from live state. Dragging a computer changes the proposed cross-computer edge mapping while retaining its internal monitor topology.

A paired layout save prepares and validates both sides against their configuration revisions and active outputs. Configuration writes retain comments, are atomic per file and preserve an initial UI backup. Success is reported only after both sides acknowledge their saved settings. The input connection is then re-established using the new entry settings. Conflicting edits are rejected. A connection failure during commit may leave confirmation ambiguous; this is reported for review after reconnection rather than hidden behind a success message.

Each screen connection has a stable ID shared by both configurations. Protocol 5
entry and return messages carry that ID and a normalized position, so array order
does not determine the destination. Return can use a different connection from
entry. The receiver selects the connection's output before placing the pointer;
held input must be released before switching outputs. A single capture worker
owns all enabled edge surfaces and permits only one active capture. Output
changes release active capture before the remaining valid connections are armed
again.

Pairing is a separate explicit operation. Public certificate data is frozen and normalized, its displayed fingerprint is bound to the import, and private keys are never exported. Importing a new peer does not silently change physical device access or firewall authorization.

## Data and dependency boundaries

Traffic uses mutually authenticated TLS 1.3 and a matching protocol/ALPN version. It contains input events, bounded device capabilities, screen metadata and required control state. No desktop video or clipboard data is transmitted. KDE Connect can continue to handle clipboard synchronization independently.

An actual uinput test found an incorrect pointer-sized `UI_SET_PHYS` request in evdev 0.13.2. The repository pins the upstream fix [7fee138a](https://github.com/emberian/evdev/commit/7fee138a341f73b81b9c4f8c59e3527c6a169ea3); it does not patch the shared Cargo cache or vendor the dependency.

The UI uses English message IDs and a complete Simplified Chinese catalog. Language changes retain drafts and do not touch backend configuration. GTK calls stay on the main thread; command and socket operations run in workers.

See [Testing](testing.md) for the distinction between pure logic tests, isolated compositors and real-device evidence. Background metadata, status or process activity alone is not an end-to-end input result.
