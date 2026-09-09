# Security

English · [简体中文](SECURITY.zh-CN.md)

NiriBridge is an early beta and does not yet have a long-term supported release or an independent security audit. Tests and real-device acceptance are documented separately.

## Implemented boundaries

- Input is carried over TLS 1.3 with mutual authentication. Only the explicitly paired certificate is trusted. Incorrect server names, untrusted servers and unpaired clients have rejection tests.
- Input frames have size, numeric-range, timing, session and sequence checks. Native touchpads have additional capability, axis and contact limits.
- Disconnects, delayed input, blocked writes, lock state and loss of capture release held keys, buttons, virtual contacts and capture resources.
- Session availability is checked against logind's user identity, Active and LockedHint state. Unknown state does not permit sharing.
- The desktop interface uses a private same-user Unix socket. It does not expose a web management port or a general input-injection API.
- Pairing imports bind the user's reviewed fingerprint to a frozen copy of the public certificate. Private-key files and self-pairing are rejected.
- Screen-setting updates are restricted to entry displays and boundary spans, with current revision checks on both sides. They do not change authentication, physical device authorization or Niri's internal monitor arrangement.
- The program does not log keyboard input, contact coordinates, clipboard contents or private keys. The diagnostic export excludes pairing addresses and device serials.

## Input access

The reviewed administrator helper grants `uaccess` for selected physical devices and uinput. The runtime service runs as the desktop user. This permission model is per user: other processes under the same account may have the same access. It does not sandbox untrusted programs running as that user.

Native touchpad mode temporarily owns selected touchpads while the paired unlocked connection is usable, mirroring input locally or forwarding it to the peer. Locking, stopping or disconnecting closes those handles and releases ownership. Virtual NiriBridge devices are rejected as physical input sources to prevent feedback.

Never run the sharing service as root, relax private-key permissions, trust an unverified pairing file or disable protocol validation to work around a connection problem.

## Configuration recovery

Configuration files are replaced atomically. Concurrent edits are detected and retained. Unfinished local transaction writes are rolled back only when the file still matches the program's own revision. If a network failure makes a paired save ambiguous, the interface reports that confirmation is missing; check both sides after reconnection.

The administrator helper retains protected recovery information and refuses to overwrite externally modified rules. Details are in [Installation](docs/setup.md).

## Reporting

Use [GitHub private vulnerability reporting](https://github.com/blackdream1890/niri-bridge/security/advisories/new) for security-sensitive reports. Include the affected version, reproduction steps and redacted diagnostics. Do not publish exploitation details, credentials, pairing files or personal diagnostics in a public issue. No response-time guarantee is currently made.

Release archives are built from a fixed source commit. SHA-256 checksums accompany the binaries and the corresponding source archive. Install the same supported version on both computers; follow the release notes when protocol compatibility changes.
