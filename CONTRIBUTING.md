# Contributing

English · [简体中文](CONTRIBUTING.zh-CN.md)

NiriBridge is an early beta licensed under GPL-3.0-or-later. Contributions are submitted under the same project license unless explicitly agreed otherwise. Preserve third-party attribution and license notices. The current development target is Ubuntu 26.04 with Niri 26.04.

## Work from demonstrated behavior

Read the [design](docs/design.md), [setup guide](docs/setup.md) and [test documentation](docs/testing.md). Distinguish protocol advertisement, isolated compositor tests and actual two-computer input behavior.

Keep the input backend independent of the desktop window. Preserve input timing, capture release, device ownership checks and paired-certificate authentication. Configuration changes must preserve comments and detect concurrent edits. A saved screen connection must be acknowledged by both computers before the interface reports success.

Do not log input events, private keys, clipboard contents or unredacted personal diagnostics. Example configurations and screenshots should use generic devices and sample data.

## Validation

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
python3 -B -m unittest discover -s scripts -p 'test_*.py'
python3 -B ui/test_i18n.py
```

The native widget tests require a GTK display and use simulated state without operating the real sharing service:

```sh
python3 -W ignore::DeprecationWarning:gi.events -B ui/test_ui.py
```

The warning filter is limited to the system PyGObject/Python compatibility warning. Application exceptions and GTK warnings remain visible.

Run the isolated Niri suites when changing capture, the coordinator or paired layout transactions:

```sh
cargo test --locked --test nested_capture -- --ignored --nocapture
cargo test --locked --test encrypted_bridge -- --ignored --nocapture --test-threads=1
```

Real-device probes require an explicitly agreed testing window. Preserve the current desktop state and restore it after each test.

## Translations

UI source strings are English. Mark user-facing text with `_()` and messages that will be translated later with `N_()`. Keep placeholders named and put complete sentences in the catalog where possible. Device names, fingerprints, protocol keys and file paths are data, not translatable messages.

The Simplified Chinese catalog is `ui/locales/zh_CN.json`. `ui/test_i18n.py` checks that every marked message has a translation and that formatting placeholders match. Verify both languages visually after changes to dialogs, buttons or screen controls. Language switching must preserve unsaved edits and must not restart the sharing backend.

## Issue reports

Include the OS, Niri and application versions, screen scaling and arrangement, input device types, reproduction steps, expected behavior and actual behavior. Report whether the failure occurs locally, remotely or in both directions.

Use the redacted diagnostics command or the interface's Copy diagnostics action. Do not attach private pairing files, input recordings, subscription URLs, device serials or unreviewed logs. See [Security](SECURITY.md) for sensitive reports.

## Release and privacy conventions

Use a GitHub username and noreply email for project commits. Keep private workstation records outside Git and review screenshots before attaching them. The [release procedure](docs/releasing.md) documents the build, checks, source archive and publication process.
