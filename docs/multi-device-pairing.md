# Saved devices and the 1.0 release

## Confirmed scope

Keep multiple explicitly trusted computers on one installation, and choose one
computer for input sharing at a time. Adding a computer must retain every
previous computer's public certificate, SHA-256 fingerprint, connection method,
address and local screen connections. The local identity and selected physical
input devices remain shared by all saved computers.

Importing a new certificate requires the existing full-fingerprint review. An
already saved certificate is recognized by its SHA-256 fingerprint; importing it
again must not reset its settings or create another entry. Different certificates
with the same device name remain separate and distinguishable. A new certificate
never silently replaces trust in an older certificate with the same name.

Adding another saved computer keeps the existing selection. The first imported
computer becomes selected when there is no existing pairing. Both endpoints must
select each other to connect. Only the selected certificate is trusted by the
running input transport; saved computers do not gain simultaneous access.

Old single-peer configurations remain readable. The first pairing-management
write retains the old peer as a saved computer without changing the local
certificate or key. Reading configuration or running diagnostics does not migrate
or write it. Screen saves and connection changes update the selected computer's
settings, and switching away and back restores them.

## Confirmed switching workflow

Switching first stops sharing and leaves it stopped. The user explicitly chooses
Start sharing for the selected computer. A failed switch also leaves sharing
stopped, while preserving the original selection and saved trust. Uncommitted
settings must be saved or explicitly discarded before switching. Active-desktop
tests require an agreed time and recovery plan.

## Storage and consistency

The configuration file holds saved-device metadata and the selected runtime
settings. Public certificates are retained in immutable, fingerprint-addressed
files beside it. Switching updates the selected certificate, name, connection
and local edges in one configuration transaction using the existing revision
check, edit lock, atomic replacement and comment-preserving TOML editor.

Saving current settings also updates the selected saved device in that same
transaction. Paired screen-layout transactions use the same code path. A failed
or interrupted write cannot leave a configuration that names one device while
trusting another device's certificate. No private key is copied or exported.

New computers start with the existing first-setup connection defaults and one
entry on a configured local display. Their connection address and screen
connections are configured independently; an earlier peer's address is never
reused as the new peer's address.

## Acceptance record

- [x] Retain legacy pairing and identity during migration (temporary-file tests).
- [x] Save two distinct peers, including equal display names, without losing trust.
- [x] Re-importing the same certificate preserves connection and screen settings.
- [x] Preserve physical devices, native touchpad preference and TOML comments.
- [x] Reject stale revisions, invalid certificates and unreviewed fingerprints.
- [x] Refuse to switch a running or independently managed backend unsafely.
- [x] Failed selection and unacknowledged screen transactions retain the old configuration.
- [x] Trust only the selected peer in loopback mutually authenticated TLS tests.
- [x] Cover saved selection, unsaved drafts and language switching in GTK tests.
- [x] Verify independent addresses and screen settings across switches in automated three-device fixtures, and retain earlier pairings during installed-device selection changes.
- [x] Run ordinary Rust/Python checks and isolated compositor/coordinator regressions in the development snapshot.
- [x] Verify candidate package installation, upgrade and retained device records.
- [x] Confirm working sharing with different selected physical pairs and receive user acceptance of the current behavior.
- [x] Review existing Niri/KDE acceptance limits and state the 1.0 validation scope.
- [x] Verify current-source privacy, dependency audit and refreshed license notices.
- [x] Verify candidate corresponding source, offline rebuilding and checksums.

The installed-device record covers three Ubuntu Wayland workstations used in
separate pairs, retained earlier peers, mutually authenticated connections,
capture readiness and observed bidirectional control and physical takeover.
The user confirmed normal operation and authorized the current 1.0 release.
This does not claim an exhaustive physical matrix of every target switch,
display layout, input method, gesture or suspend/recovery combination. Those
remain part of continued hardware coverage. Earlier basic KDE/Niri keyboard and
pointer acceptance is retained in the testing record. The current 1.0 physical
rollout covers Niri; KDE hardware scenarios were not revalidated in this rollout.

## Verification record

The 1.0.0-rc.1 development snapshot passed formatting, strict Clippy, 66 ordinary
Rust tests, 31 Python script tests, translation checks and 31 GTK tests in each
language. Dedicated isolated suites passed the kernel/libinput handoff, KWin,
Niri and encrypted paired-screen coordinator checks. Three private D-Bus Portal
cases cover consent, denial and absent pointer permission, including real file
descriptor transfer without using input devices. The optimized build passed.

The security refresh uses rustls 0.23.46, rustls-webpki 0.103.16, Tokio 1.53.3
and zbus 6.2.0, removing the retired standalone zvariant crates. The current
locked graph passes `cargo-audit audit --deny warnings`; license notices are
regenerated and checked. Candidate 1 was then committed as `fb95ccf` and passed
the clean-commit package pipeline, including archive and Debian installation,
upgrade/removal smoke checks and an offline corresponding-source rebuild.
Physical pair changes and user acceptance were subsequently recorded during
the release-candidate rollout.

Candidate 2 added guided LAN permission review, copyable local addresses,
unsaved-setting protection and invalid destination checks. Its clean commit
`01b8fc3` passed 71 ordinary Rust tests, 53 Python script tests, translation
checks and 61 GTK tests in each language. The complete container pipeline also
passed isolated kernel/libinput, KWin, Niri and encrypted coordinator suites,
dependency audit and license checks, installation/upgrade/removal checks and
an offline corresponding-source rebuild.

Version 1.0.0 promotes that accepted behavior without a new input or pairing
implementation. Its exact clean commit must pass the checked-source release
pipeline again before publication. Verify the remote tag, source identity,
downloaded checksums and release provenance before publishing the draft.
Compilation and isolated tests do not replace the physical and human evidence
recorded above.
