# Changelog

## 0.2.0-beta.3 — Unreleased

- Opening the interface leaves sharing stopped until the user starts it. Stopping sharing releases input; quitting from the tray waits for sharing to stop before closing the application.
- Physical touchpad handoff restores a coherent neutral state to the compositor instead of only releasing the evdev grab. An isolated Linux-kernel test checks all initial, captured and next contact-slot combinations, including contacts held at stop. A real libinput observer checks pointer motion, three/four-finger gestures and input leakage; a separate case reproduces and repairs legacy ghost contacts.
- Ubuntu `.deb` packages install runtime dependencies and apply updates when the desktop application is reopened. The portable installer uses the same safe lifecycle.
- Upgrades automatically back up previous application files and settings, preserve identities and custom files, and leave sharing stopped. Login startup now opens only the interface.
- Legacy touchpad input can be restored without restarting the desktop or rebinding the hardware driver. Protocol mismatches are reported separately from ordinary connection failures.
- Multiple user-defined screen connections between the same two computers, with independent displays, edges and ranges. A crossing can return through a different connection.
- The GTK editor adds, selects and removes connections without overwriting other drafts; numbered, colored endpoints identify each pair.
- Stable connection IDs select the correct target display and return position. Capture uses one worker with multiple edge surfaces, allowing only one active capture.
- Existing single-edge configurations remain readable. Saving converts them to the multiple-edge format while preserving comments and unrelated settings.
- Overlapping ranges on the same display edge are rejected. Missing displays disable the affected connection while other matched connections remain usable.
- Completed configuration transactions explicitly release their file lock, including when a concurrent subprocess inherited a descriptor before exec.
- Local agent instructions are excluded from published source. Contribution and release guides clarify public attribution choices.

This version uses **input protocol 5** and must be installed on both computers.
Keep a configuration backup before upgrading: earlier versions do not read the
new `[[edges]]` format. Physical acceptance of multiple connections on the initial
multi-monitor setup is still pending; isolated two-connection tests cover both
directions, different return edges, mapped positions and held-key cleanup.

## 0.2.0-beta.1

First public beta, targeting Ubuntu 26.04, Niri 26.04 and Linux x86_64.

- Bidirectional keyboard and pointer sharing, including native three/four-finger touchpad gestures that follow the pointer.
- Touchpad frame timestamps and asynchronous input readiness preserve pointer responsiveness.
- Paired-certificate TLS 1.3, bounded input handling, session-lock checks and recovery of held input state.
- Native GTK interface for connection status, pairing, selected input devices, startup preferences and synchronized screen-entry settings.
- English source interface and complete Simplified Chinese translation; language changes preserve unsaved edits.
- User installation, previewable removal, retained user configuration and explicit administrator authorization for input permissions.
- GPL-3.0-or-later licensing, retained dependency notices, reproducible build instructions and corresponding-source archives with locked dependencies.
- Migrated the retired rustls-pemfile wrapper to the supported rustls PEM interface.

### Compatibility and limits

Both computers should install the same release. This beta uses input protocol 4.
Other compositors and operating systems are not yet validated. Long suspend cycles,
a wider range of physical devices, complete input-method editing scenarios and
all combined gestures still need broader acceptance. Certificates are not renewed
automatically. See the setup and testing guides before deploying updates.
