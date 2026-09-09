# Requirements and acceptance

English · [简体中文](requirements.zh-CN.md)

## Confirmed behavior

- The initial target is two Ubuntu computers running Niri on Wayland, preserving their existing desktop environments.
- A laptop keyboard and touchpad control the desktop after the pointer crosses to it. The desktop's connected keyboard and mouse can control the laptop in the other direction.
- The keyboard follows the pointer automatically. Physical devices remain connected to their original computer.
- Returning to the input source restores local control. Niri shortcuts and application keyboard input must work on either destination.
- Physical input on the receiving computer ends sharing and restores local control. Injected remote events must not trigger physical takeover.
- Locking either session pauses sharing and releases capture and simulated held states. Unlocking permits a new crossing without restoring the earlier exclusive capture automatically.
- Native touchpad gestures, including three-finger workspace switching and four-finger overview, follow the pointer's destination.
- Existing KDE Connect clipboard sharing can continue. NiriBridge does not add another clipboard implementation or depend on KDE Connect for input sharing.
- A native desktop interface provides screen connections that can be adjusted on one computer and synchronized to both.
- Multiple configurable edge pairs connect the same two computers. Each has independent displays and ranges, and can be used for entry or return in either direction.
- The interface uses English source text, a complete Simplified Chinese translation, system locale selection and a manual language choice for English-speaking and Chinese-speaking users.

## Configuration and community scope

Hostnames, addresses, output names, resolution, scaling and physical devices must be configurable or discovered from actual capabilities. Examples use generic names and fictional addresses. Personal acceptance records are kept in the ignored `.local/` directory.

The initial multi-monitor acceptance arrangement has two desktop monitors, one in portrait orientation, and a laptop below the landscape monitor. This is a test arrangement, not a hardware restriction. Crossings use logical coordinates and normalized boundary ranges, including scaling and rotation.

The first supported topology is a pair of Niri computers. Other compositors, distributions and multi-computer graphs require their own validation. Dependency sources and licenses must be traceable, and build and test commands must be runnable. The project uses GPL-3.0-or-later. Releases follow the documented build, privacy and acceptance checks.

## Initial boundary decisions

The initial two-computer arrangement connects part of the landscape display's bottom edge with the laptop's top edge. The selected physical devices and entry ranges are installation-specific. The emergency return chord is Ctrl+Alt+Shift+Escape.

The screen interface changes the crossing between computers; each computer's internal monitor placement continues to follow Niri. Simultaneous control requests are handled conservatively by rejecting re-entry and returning local control; broader contention behavior needs continued acceptance.

## Acceptance evidence

The user confirmed the basic bidirectional input behavior. Keep every additional item's validation boundary explicit; isolated tests do not replace real-device acceptance.

- [x] Laptop keyboard and touchpad control the desktop and return normally.
- [x] Desktop keyboard and mouse control the laptop and return normally.
- [x] Keyboard follows the pointer. Isolated tests also check that the source client does not receive keys intended for the destination.
- [x] Three/four-finger gestures follow the target. Native gesture probes passed and the user confirmed normal basic operation.
- [x] Touchpad responsiveness returned to normal after the timestamp and readiness correction, as confirmed by the user.
- [x] Unauthenticated peers cannot control input. Tests cover incorrect server names, untrusted servers and unpaired clients.
- [ ] Complete local movement across both desktop monitors.
- [ ] Full Niri shortcut, Chinese editing, modifier and application shortcut scenarios on both computers.
- [ ] All mouse buttons, side buttons, wheel, touchpad taps and two-finger scrolling.
- [ ] Consistent held-key and held-button behavior during all crossings and dragging scenarios.
- [ ] Real-device network/process failure recovery and independent emergency return.
- [ ] Long suspend recovery, input hotplug and display reconfiguration.
- [ ] Broader verification that injected virtual input never returns as physical input.
- [ ] Bidirectional KDE Connect copy/paste acceptance alongside sharing.
- [ ] End-to-end latency and jitter measurements, including high-rate mouse input.
- [x] Actual screen-entry changes initiated on either installed desktop updated both configuration files; original files were restored byte-for-byte and both sides reconnected.
- [x] Isolated two-connection tests cover both directions, return through a different connection, mapped return positions and held-key cleanup.
- [ ] Multiple connections across the physical landscape, portrait and laptop displays.

Version 0.2 also has widget, translation and paired configuration tests. Real tray open/pause/start/quit behavior, status loading, lock-state controls and preservation of settings/startup state were checked on both installed desktops. Paired-file transaction tests use isolated Niri sessions and mutually authenticated TLS. See [Testing](testing.md) for commands and limitations.

## 0.2.0-beta.3 lifecycle acceptance

- [x] Opening the interface does not start sharing; Start/Stop controls are explicit.
- [x] Tray quit waits for stop success; failure keeps the interface open.
- [x] Window close retains the tray and current sharing state.
- [x] Isolated kernel/libinput checks cover ordinary handoff and legacy ghost-contact recovery.
- [x] Installation tests cover backups, startup migration, old interfaces and customized files.
- [ ] Two physical desktops: repeated start/stop, tray quit and upgrade to the release candidate.
