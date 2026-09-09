# Third-party components

NiriBridge's original code and artwork are licensed under GPL-3.0-or-later.
Third-party software retains its own copyright and license terms.

## Rust dependencies

The exact Linux x86_64 dependency versions and retained notices are recorded in
[licenses/rust/index.json](licenses/rust/index.json). Complete corresponding
license texts and attribution notices are included beside that index and in the
installed documentation. They are generated from the locked dependency graph,
with reviewed license choices in `about.toml`.

The evdev dependency is pinned to the upstream `UI_SET_PHYS` correction at
[7fee138a341f73b81b9c4f8c59e3527c6a169ea3](https://github.com/emberian/evdev/commit/7fee138a341f73b81b9c4f8c59e3527c6a169ea3).
Its upstream copyright and Apache-2.0/MIT licensing remain intact.

The ring/BoringSSL assembly also embeds a public upstream authorship string.
The reviewed attribution is recorded in [licenses/binary-attributions.json](licenses/binary-attributions.json); it is retained as upstream attribution, not treated as maintainer-private contact information.

## System components

Python, GTK, PyGObject, Cairo, OpenSSL, systemd, udev and Niri are supplied by the
operating system or installed separately. The NiriBridge archive does not bundle
these system components or change their licenses. Consult their installed
copyright files and upstream sources for their own terms.

Niri and Weston used in isolated tests are build/test tools, not components of the
NiriBridge release archive. Their source versions are pinned in the build image.

## Protocols and artwork

The desktop tray implementation follows the StatusNotifierItem and DBusMenu
interfaces. It uses GIO; no AppIndicator implementation is copied into this tree.
The NiriBridge SVG icon is project artwork and is covered by the project license.
UI examples and published screenshots use synthetic device and pairing data.
