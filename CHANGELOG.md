# Changelog

## 0.3.0-beta.3

- Keep native pairing-file dialogs alive until the user selects a file or cancels, fixing export and import buttons that appeared to do nothing on Niri and KDE.
- Reuse the pending chooser on repeated clicks and close it when its parent is destroyed; late responses cannot export or initiate pairing.
- Add real GTK chooser regressions for public-certificate export, import review, cancellation, reopening and parent cleanup. Fingerprint confirmation remains mandatory.

## 0.3.0-beta.2

- Update rustls to 0.23.45 to address RUSTSEC-2026-0285; keep the release audit gate enabled.

- Automatically detect Niri and KDE Plasma Wayland from the active compositor socket.
- Discover KDE logical output geometry, including fractional scaling and rotated displays.
- Add a pointer-only RemoteDesktop Portal / libei backend with explicit desktop consent, revocation handling and no screen or clipboard capture.
- Share the existing capture, paired TLS, recovery, keyboard and native touchpad paths across both desktops.
- Identify graphical sessions from logind when launched by the user service; check KWin virtual-device readiness without accessing its protected process file descriptors.
- Keep KDE Breeze title-bar controls at their native proportions with visible icons, remove the inherited light top border, and use explicit tray pixels on fresh installations.
- Add an isolated two-output KWin capture/injection and authorization-revocation regression, including a stale Niri environment. KDE X11 remains unsupported.


## 0.2.0-beta.3 — 2026-09-09

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
new `[[edges]]` format. Both initial desktops passed upgrade, repeated Start/Stop, window hiding, tray
quit, idle reopening and reconnection checks. The user confirmed normal local
touchpad movement and three/four-finger gestures after stopping, plus both
physical screen connections. Isolated tests also cover different return edges,
mapped positions and held-key cleanup. Broader hardware and long suspend
acceptance remain open.

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
