# Changelog

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
