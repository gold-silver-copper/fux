# A Linux environment for fux's tests, the fux-fuzz harness and the repro
# scripts. See run.sh; nothing here is published or used by the crate.
#
# libasound2-dev is here only because fux does not build without it on Linux:
# bevy_remote 0.20 depends on bevy_dev_tools, which depends on bevy_audio and
# through it on ALSA. That is hunt 7 finding 009 (fux-fuzz/BREAKS.md); remove
# the package when it is fixed, and the build here is the check.
FROM rust:1.95-bookworm
RUN apt-get update \
 && apt-get install -y --no-install-recommends python3 lsof procps psmisc perl bash util-linux \
      e2fsprogs \
      pkg-config libasound2-dev \
 && rm -rf /var/lib/apt/lists/*
# The repository's rust-toolchain.toml pins the toolchain; install it once in
# the image rather than on every run.
COPY rust-toolchain.toml /tmp/toolchain/rust-toolchain.toml
RUN cd /tmp/toolchain && rustup show active-toolchain || rustup toolchain install \
 && cd / && rustup toolchain list && chmod -R a+w /usr/local/rustup
# Tests assume an ordinary user: root ignores the socket's file permissions,
# which are fux's access control.
RUN useradd --create-home --uid 1000 fux \
 && mkdir -p /cargo-home /target \
 && cp -r /usr/local/cargo/bin /cargo-home/ \
 && chown -R fux:fux /cargo-home /target
USER fux
ENV CARGO_HOME=/cargo-home CARGO_TARGET_DIR=/target PATH=/cargo-home/bin:/usr/local/cargo/bin:$PATH
WORKDIR /src
