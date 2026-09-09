# SPDX-License-Identifier: GPL-3.0-or-later
FROM ubuntu:26.04@sha256:2260313b31c8c011cd2eebe728008efac1b3982be73eb71348ea2648d2c0e09b

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_HOME=/opt/cargo \
    RUSTUP_HOME=/opt/rustup \
    CARGO_BUILD_JOBS=2 \
    PATH=/opt/tools:/opt/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential ca-certificates curl git pkg-config rustup \
    python3 python3-gi python3-gi-cairo python3-cairo gir1.2-gtk-3.0 \
    openssl dbus-x11 xvfb xauth desktop-file-utils weston acl polkitd systemd \
    libgl1-mesa-dri libegl1 libgles2 libinput-dev libgbm-dev libseat-dev \
    libudev-dev libxkbcommon-dev libwayland-dev libegl-dev libpango1.0-dev \
    libcairo2-dev libdisplay-info-dev libpipewire-0.3-dev libclang-dev \
    && rm -rf /var/lib/apt/lists/*
RUN rustup toolchain install 1.98.1 --profile minimal --component rustfmt --component clippy \
    && rustup default 1.98.1

# Build the official Niri 26.04 source for isolated tests. Never use --session
# or attach these test processes to the host user's display or input devices.
RUN git init /tmp/niri \
    && git -C /tmp/niri fetch --depth 1 https://github.com/niri-wm/niri.git 8ed0da44d974c32c6877d2f4630c314da0717ecb \
    && git -C /tmp/niri checkout --detach FETCH_HEAD \
    && test "$(git -C /tmp/niri rev-parse HEAD)" = 8ed0da44d974c32c6877d2f4630c314da0717ecb \
    && CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_PROFILE_RELEASE_LTO=false cargo build --manifest-path /tmp/niri/Cargo.toml --locked --release --bin niri \
    && install -m 0755 /tmp/niri/target/release/niri /usr/local/bin/niri \
    && rm -rf /tmp/niri

COPY packaging/tools.json /opt/tool-source/packaging/tools.json
COPY scripts/fetch-tools.py /opt/tool-source/scripts/fetch-tools.py
RUN python3 /opt/tool-source/scripts/fetch-tools.py --destination /opt/tools \
    && rm -rf /opt/tool-source \
    && useradd --create-home --uid 10001 tester
# Optional desktop fonts are explicit so both language screenshots are legible
# in the otherwise minimal test image. pkexec is a separate Ubuntu package.
RUN apt-get update && apt-get install -y --no-install-recommends \
    fonts-noto-core fonts-noto-cjk pkexec shellcheck \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /workspace
