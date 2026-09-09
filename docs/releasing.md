# Release procedure

Only publish from reviewed source with public commit metadata. Keep private
configuration, credentials, raw diagnostics and personal build paths out of Git,
release assets, screenshots and CI artifacts.

## Identity and provenance

Use the maintainer's GitHub username and GitHub noreply email for release commits.
Check the complete history with `python3 scripts/check-public.py --history` before
making a repository public. An existing private repository can contain sensitive
metadata even when its current tree is clean; retain that history privately and
prepare a clean publication repository when necessary. Do not merge private
history back into the public repository.

`LICENSE`, `COPYRIGHT`, Cargo metadata and UI notices declare GPL-3.0-or-later.
`licenses/rust/` retains dependency license texts and upstream attribution.
Regenerate notices after dependency changes:

```sh
python3 scripts/fetch-tools.py --destination .local/tools
python3 scripts/third_party.py --cargo-about .local/tools/cargo-about
```

Review changed license choices rather than expanding `about.toml` simply to make
a failed check pass. Third-party copyright statements are retained; they are not
replaced with the project maintainer's authorship.

## Build and verification

The Dockerfile pins the Ubuntu image digest, Rust toolchain, official Niri source
commit and audit-tool archive hashes. It uses an isolated Niri/Weston environment,
with no host input devices or desktop sockets mounted.

From a clean source commit, use an empty output directory:

```sh
docker build --tag niri-bridge-build:release --file packaging/ubuntu26.04.Dockerfile .
mkdir dist
docker run --rm --env NIRI_BRIDGE_CI=1 \
  --mount type=bind,source="$PWD",target=/workspace,readonly \
  --mount type=bind,source="$PWD/dist",target=/output \
  niri-bridge-build:release bash scripts/ci.sh
```

The pipeline checks formatting, Clippy, ordinary Rust tests, Python installation
and privacy tests, translations, both UI languages, isolated input recovery and
paired screen updates. It audits the locked dependencies, checks license notices,
builds a release and tests installation, first pairing, upgrade and removal under
a separate non-root account. It also rebuilds the corresponding-source archive
with offline Cargo dependencies.

The non-root container check does not grant real physical-device permissions or
start a real graphical login session. Real logind, input authorization, lock,
recovery and desktop behavior require separately recorded physical acceptance.

Archive timestamps, ownership and file order are normalized. Personal build
paths are remapped before compilation. This documents a repeatable build process;
it does not claim bit-for-bit identical executables on arbitrary build hosts.

## Artifacts

Every release contains:

- A Ubuntu 26.04 x86_64 binary archive with the backend, GTK interface, installation
  and removal tools, documentation and license notices.
- A corresponding-source archive with the exact committed project files, locked
  Rust dependency sources, and an offline Cargo vendor configuration.
- `SHA256SUMS` covering both archives.

`release.json` records the source commit, version, target, compiler and Cargo.lock
hash without collecting usernames, machine names, network addresses or input data.
Vendored upstream source is checked against its Cargo file checksums. Public
upstream test fixtures and attribution are retained; local private audit rules
can additionally reject exact maintainer-private values without printing them.

Published screenshots are generated from `ui/render_demo.py` using synthetic
state on an isolated display. Review both languages and check image metadata.
Never substitute screenshots of personal pairing files or the active desktop.

## GitHub publication

Choose a semantic version, update Cargo metadata and CHANGELOG, and complete the
checks on that exact commit. An early release uses a prerelease version such as
`0.2.0-beta.1`; the tag must match it exactly as `v0.2.0-beta.1`.

The Release workflow builds an existing version tag, verifies the full public
history, and prepares a draft with checked assets. For public repositories it
also records GitHub build provenance attestations. Workflow dispatch must run
from the same commit as the requested tag so the attestation identifies the
correct source.

Before publishing the draft, verify the target commit, checksums, installation
instructions, prerelease status, known limitations and private vulnerability
reporting. Publish only the reviewed draft. Do not replace files in an existing
published version; issue a new version for changed artifacts.
