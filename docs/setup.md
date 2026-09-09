# Installation and usage

English · [简体中文](setup.zh-CN.md)

The current development target is Ubuntu 26.04 and Niri 26.04. Install the same NiriBridge version on both computers.

## Build and install

Niri 26.04 must already be installed separately; see the [official Niri project](https://github.com/niri-wm/niri). NiriBridge does not install or reconfigure the compositor.

The desktop interface uses the system Python, GTK 3, PyGObject and Cairo. Its tray integration uses GIO and the StatusNotifierItem protocol; no additional AppIndicator binding is required.

For the easiest Ubuntu installation, download and verify the `.deb` release. Open it with the system package installer, or run:

```sh
sudo apt install ./niri-bridge-0.2.0-beta.3-ubuntu26.04-amd64.deb
```

Open NiriBridge from the application launcher to complete setup as your desktop user. The package supplies runtime dependencies, keeps the shared release in `/usr/lib/niri-bridge`, and applies updates to each user's installation when the application is next opened. Package maintenance never restarts a running sharing session. An unchanged launcher from an older portable installation is redirected to this update entry point; customized launchers are preserved. If you customized yours, open `/usr/bin/niri-bridge-ui` to apply the package update.

For a portable archive or source checkout, install the runtime dependencies with:

```sh
sudo apt install python3 python3-gi python3-gi-cairo gir1.2-gtk-3.0 openssl acl pkexec
```

Run the NiriBridge installer as your desktop user, without sudo. Administrator authentication is requested separately when granting input-device access. The backend build needs a C compiler and the Rust toolchain pinned by the repository. The `openssl` command is used to inspect and normalize public pairing files.

For the prebuilt release archive, verify `SHA256SUMS`, extract the complete archive, and run:

```sh
python3 scripts/install.py
```

For a Git source checkout, build first with `cargo build --locked --release`. The separate source release archive contains `vendor/` and `.cargo/config.toml`, allowing `cargo build --frozen --release` without downloading Rust dependencies. Install the pinned Rust toolchain and system packages beforehand.

Launch **NiriBridge** from your application launcher or run:

```sh
niri-bridge-ui
```

The installer places the backend and launcher in `~/.local/bin`, UI resources in `~/.local/share/niri-bridge/ui`, and the desktop entry and icon in the standard user application directories. It installs the backend and interface user services, updating its own unchanged units and preserving customized units. It never replaces your identity, paired certificate, input permissions or configuration.

Installation waits for the interface to close, stops sharing safely and leaves it stopped. An older interface without the new quit action must be saved and closed from its tray menu before retrying installation. The old `--no-restart` option is accepted for script compatibility, but installation no longer starts sharing automatically.

Protocol or ALPN mismatches are rejected, so update both computers together.

## Interface language

English is the source and fallback language. Simplified Chinese is fully translated. By default the interface follows the desktop's message locale; select **English**, **简体中文** or **System default** in Preferences. The first-run setup also provides this choice.

Minimal installations also need a CJK font to display Chinese; install `fonts-noto-cjk` if Chinese characters appear as boxes.

Changing language recreates the interface while retaining unsaved settings, screen edits and the current page. It does not restart sharing. The local language preference is stored separately in `ui-preferences.json` next to the UI configuration location.

For an explicit launch-time choice:

```sh
niri-bridge-ui --language en
niri-bridge-ui --language zh_CN
```

## Initial setup and pairing

On first launch, choose a lowercase device name, an active entry display and the physical devices to share. NiriBridge creates a local identity without replacing an existing key. An existing configuration is loaded automatically.

In **Devices & pairing**:

1. Export this computer's public pairing file.
2. Transfer it to the other computer, and import that computer's exported file here.
3. Compare every group of the displayed SHA-256 fingerprint with the other computer's interface.
4. Confirm trust only when they match. Pairing allows shared input and synchronization of screen entry settings.

An imported file is checked again against the reviewed fingerprint before it is saved. Private-key files and the local computer's own certificate are rejected. Only public certificates are exported.

The underlying files are `identity.pem`, `identity.key.pem` and `peer.pem` in `~/.config/niri-bridge`. The directory is private to its owner; the private key has mode 0600. Current generated certificates last one year and should be renewed and re-paired before expiry.

## Connection and input permissions

Choose **Wait for the other computer to connect** on one computer. Its usual listening address is `0.0.0.0`. On the other, choose **Connect to the other computer** and enter an address reachable over the LAN. Both use TCP port 42420 by default.

Select the physical keyboard, mouse and touchpad devices in Preferences. A disconnected configured device remains in the selection so it is not silently removed. Native touchpad gestures require the touchpad's stable path to be selected and **Share native gestures** enabled.

The **Allow device access** action uses the existing administrator installer. Review the scope before completing the operating system's administrator authentication. Permissions are limited to selected device identities and uinput, using `uaccess` for the active desktop user; the program does not run as root or add the user to the entire input group.

For reviewed command-line preparation:

```sh
python3 scripts/device-access.py plan --config ~/.config/niri-bridge/config.toml --output ~/.config/niri-bridge/device-access.json
sudo python3 scripts/device-access.py install --plan ~/.config/niri-bridge/device-access.json
```

If the listening computer's UFW is active, prepare its plan with `--allow-from OTHER_COMPUTER_LAN_IPV4` to allow TCP 42420 from that one private IPv4 address. The installer preserves firewall enablement and does not open the port to an entire network. Existing installations with a different authorization plan are deliberately refused; review and update the installation scope before changing it.

The managed rule is `/etc/udev/rules.d/71-niri-bridge-input.rules`. Protected recovery information is stored in `/var/lib/niri-bridge/device-access.json`.

## Screen connections

Once both computers are connected and unlocked, open **Screen connections** on either one.

- Select a connection from the list, or use **Add connection** to create another pair of entry edges.
- Choose each endpoint's display, edge and range independently. Vertical ranges run from top (0%) to bottom (100%); horizontal ranges run from left (0%) to right (100%).
- Dragging the other computer's screen group adjusts the selected connection. Other connections retain their settings.
- Matching numbers and colors mark the paired endpoints. **Remove connection** removes the selected pair from the draft; at least one connection is retained.
- Select **Save on both computers** to apply the complete set. **Discard changes** restores the last saved set.

For example, keep a landscape monitor's bottom edge connected to a laptop's top
edge, and add the lower part of a portrait monitor's right edge connected to the
laptop's left edge. Both connections work in both directions, including entering
through one and returning through the other.

Up to 16 connections are supported between the same two computers. Ranges on the
same display edge must not overlap; adjacent ranges and different edges are
allowed. If a display is unavailable, its connections remain configured but are
inactive; other matched connections can still be used. Select an available
display or remove the unavailable connection before saving a new set.

Saving briefly restores local control, validates both configurations and writes the new entry settings. The connection is re-established using the new settings. Changes in another interface or an editor are detected by configuration revisions and are not silently overwritten. If a network interruption prevents confirmation, check the displayed settings after reconnection before assuming the change completed.

Each computer's internal monitor positions remain managed by Niri. This interface configures the crossing between the computers.

Multiple connections require protocol 5, so upgrade both computers together.
The old single `[edge]` configuration loads as one connection. Saving uses
`[[edges]]` entries with matching IDs on both sides; the interface manages these
IDs. Preserve a pre-upgrade configuration copy for rollback because earlier
versions do not read the new format. See [the configuration example](../examples/bridge.toml).

## Running and returning control

Opening the interface does not start sharing. **Start sharing** starts `niri-bridge.service`; **Stop sharing** releases input and leaves the interface open. **Quit NiriBridge** from the tray waits for sharing to stop before closing the application. If stopping fails, the interface stays open and explains that sharing could not be stopped.

Closing the window hides it in the tray and retains the current sharing state. Without a tray host, closing the window follows the quit-and-stop behavior. A tray host such as Waybar is needed for the icon. **Open NiriBridge when I log in** enables only `niri-bridge-ui.service`. Existing automatic backend startup is migrated to opening the interface; click Start sharing when ready.

**Ctrl+Alt+Shift+Escape** returns to the source computer independently of a network reply. Ordinary Escape is forwarded. Physical input on the receiving computer ends the shared session. Locking either computer pauses input and releases capture and held states.

```sh
systemctl --user stop niri-bridge.service
systemctl --user start niri-bridge.service
```

Native touchpad mode takes over the selected touchpad while the paired, unlocked connection is available, routing its frames to a local or remote virtual touchpad. Initial takeover waits for all fingers and touchpad buttons to be released. Crossings preserve current contact state; stopping or disconnecting restores a neutral contact and axis state before returning device access to the original compositor reader. The destination's Niri `input { touchpad { ... } }` settings control tapping, natural scrolling and acceleration.

## Diagnosis and recovery

The interface can copy the existing `doctor` report, which omits input contents, private keys, pairing addresses and device serials. Command-line checks remain available:

```sh
niri-bridge doctor --json
niri-bridge check-config --config ~/.config/niri-bridge/config.toml
niri-bridge check-session
```

`verify-input` and the touchpad example are explicit input tests, described in [Testing](testing.md). Do not treat protocol advertisement or process activity as proof of successful input sharing.

The first configuration modified through the UI is preserved as `config.before-ui.toml`. Undoing an input-permission installation requires administrator authentication:

```sh
systemctl --user disable --now niri-bridge.service
sudo python3 scripts/device-access.py uninstall
```

The installer preserves externally changed rules. It restores saved ACLs only for unchanged device nodes in the same boot; follow its reboot instruction when old ACLs cannot safely be reapplied. Keep or delete identity files only as an explicit owner decision.

## Current limits

- Linux keyboard codes 1–255 are supported; higher-numbered keys are not yet supported.
- Native slot-based touchpads and three/four-finger gestures are validated on the initial hardware. Pinch combinations, cross-computer dragging and long suspend recovery need broader testing.
- The first topology is two Niri computers. Other compositors, platforms and multi-computer graphs are not yet validated.
- Certificate renewal is not automated. This release is an early beta under GPL-3.0-or-later.

## Upgrade and uninstall

Install the new `.deb` on both computers and reopen NiriBridge. With a portable archive, run `python3 scripts/install.py` from each new archive. Save open edits and quit an older interface from its tray if prompted. Keep fingers off the touchpad during the brief input handoff. When both computers show the new version, click **Start sharing** on each.

Before replacing files, the installer makes a private backup under `~/.local/state/niri-bridge/backups/`. It includes the old application files, current configuration and service definitions, including edited application files; private pairing keys are retained in place. Backups are not automatically deleted. Settings, identities and custom service files are preserved, and the installer records hashes for later removal. Protocol 5 and `[[edges]]` configuration require the same version on both computers; an old version needs its corresponding configuration backup for rollback.

For a `.deb` installation, remove NiriBridge through the system package manager or `sudo apt remove niri-bridge`. Save edits and quit its tray first. This also removes unchanged per-user package installations while retaining user settings, identities, backups and input authorization. Use the following commands for a portable installation:

Preview removal before executing it:

```sh
niri-bridge-uninstall --dry-run
niri-bridge-uninstall
```

Application removal waits for the interface to close and sharing to stop. It preserves configuration, pairing files and edited application files, and removes only unchanged service definitions it owns. A manually started backend or an unrelated active configuration blocks removal until it is stopped; custom service definitions are retained.

To also revoke the managed input authorization, request administrator authentication explicitly:

```sh
niri-bridge-uninstall --revoke-input-access
```

This runs the administrator helper before removing it. A failed or canceled authorization step leaves application files in place; sharing may already have been stopped. If authorization was retained during removal, the original release archive still contains `scripts/device-access.py` for later recovery. Do not delete pairing identities unless you intend to create a new pairing.
