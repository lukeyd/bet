# Reproducible build + verify environment for the `bet` toolchain and the DOOM port.
#
# Pins the second platform the project has never been exercised on: Ubuntu 22.04 / x86_64
# (the author built everything on macOS/ARM64). Gives a sudo-free, disposable environment with
# Rust (pinned by rust-toolchain.toml) + LLVM 18 (the `backend --features llvm` dependency).
#
# The image contains NO project source — the repo is bind-mounted at /work at run time (see
# compose.yaml), so a code change never means an image rebuild.
#
# Base image pinned by digest for a fully reproducible chain (sec #45). The digest is the
# `ubuntu:22.04` linux multi-arch manifest list (OCI image index) — arch-independent, so the
# amd64 image the build actually targets is still selected underneath it. Resolved with
# `docker buildx imagetools inspect ubuntu:22.04` (cross-checked via the registry
# Docker-Content-Digest header); re-pin from the same command on a deliberate base bump.
FROM ubuntu:22.04@sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982

SHELL ["/bin/bash", "-o", "pipefail", "-c"]
ENV DEBIAN_FRONTEND=noninteractive

# 1) Base build tooling + the system libraries LLVM's static libs (linked by llvm-sys) and the
#    final compiled programs reference. The `gg-desktop` block (alsa/x11/wayland/xkbcommon) is
#    only needed to *build* the windowed runtime (minifb + cpal); the headless correctness path
#    does not use it, but installing it keeps the desktop build available too.
#    `gdb` is here so the x86_64 compiler segfault (#111) can be caught under a debugger and a
#    backtrace captured (`gdb -batch -ex run -ex bt ... --args target/debug/bet build …`); it is a
#    small base-layer tool and belongs with the rest of the build/debug tooling.
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl gnupg git build-essential pkg-config cmake xz-utils gdb \
      zlib1g-dev libzstd-dev libffi-dev libxml2-dev libedit-dev libncurses5-dev \
      libasound2-dev libx11-dev libxkbcommon-dev libwayland-dev \
 && rm -rf /var/lib/apt/lists/*

# 2) LLVM 18 from the official apt.llvm.org repo (jammy). Provides llvm-config-18 and the static
#    archives that llvm-sys 180 links against, plus clang/lld. Its own layer so tweaking the
#    app-dependency list above never re-downloads ~half a gigabyte of LLVM.
# #45: the apt.llvm.org signing key is now VENDORED at docker/llvm-snapshot.gpg.key and COPYed in
# below, instead of being fetched over TLS and trusted blindly — that removed the last build-time
# network fetch a compromised mirror or MITM (even one with a valid cert) could swap. Defence in
# depth: the vendored key is STILL dearmored and its fingerprint verified against a pinned,
# out-of-band value (fail closed) before it becomes the signed-by keyring for ~0.5 GB of LLVM.
# LLVM_KEY_FPR is the published fingerprint of the apt.llvm.org archive signing key
# (key 0x15CF4D18AF4F7421, listed at https://apt.llvm.org/); the vendored key bytes were confirmed
# to carry exactly this fingerprint before commit.
ARG LLVM_KEY_FPR=6084F3CF814B57C1CF12EFD515CF4D18AF4F7421
COPY llvm-snapshot.gpg.key /tmp/llvm-snapshot.gpg.key
RUN gpg --dearmor -o /usr/share/keyrings/llvm.gpg /tmp/llvm-snapshot.gpg.key \
 && got_fpr="$(gpg --show-keys --with-colons /usr/share/keyrings/llvm.gpg \
        | awk -F: '/^fpr:/ {print $10; exit}')" \
 && if [[ "$got_fpr" != "$LLVM_KEY_FPR" ]]; then \
      echo "FATAL: apt.llvm.org key fingerprint mismatch: got '$got_fpr' want '$LLVM_KEY_FPR'" >&2; \
      exit 1; \
    fi \
 && rm -f /tmp/llvm-snapshot.gpg.key \
 && echo "deb [signed-by=/usr/share/keyrings/llvm.gpg] http://apt.llvm.org/jammy/ llvm-toolchain-jammy-18 main" \
      > /etc/apt/sources.list.d/llvm18.list \
 && apt-get update \
 && apt-get install -y --no-install-recommends \
      llvm-18 llvm-18-dev llvm-18-tools libpolly-18-dev clang-18 lld-18 \
 && rm -rf /var/lib/apt/lists/*

# 3) Rust, pinned to the repo toolchain. Installed under /usr/local so any UID can use it; the
#    trees are made world-readable/executable (a+rX, NOT a+w — see #45) so a non-root runtime user
#    (see `user:` in compose.yaml) can *use* the toolchain without being able to overwrite its
#    binaries. The runtime cargo registry cache is redirected to the writable bind mount (CARGO_HOME
#    is overridden at run time in compose.yaml), so /usr/local/cargo itself need not be writable.
#    RUSTUP_HOME/CARGO_HOME/PATH are baked in as ENV so the proxies resolve regardless of who runs them.
ARG RUST_VERSION=1.96.1
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    LLVM_SYS_180_PREFIX=/usr/lib/llvm-18 \
    PATH=/usr/local/cargo/bin:/usr/lib/llvm-18/bin:/usr/local/bin:/usr/bin:/bin
# #45: `curl https://sh.rustup.rs | sh` executes an unverified, network-fetched script. Instead,
# download the pinned rustup-init binary for this platform, verify it against a pinned SHA-256
# (fail closed) via `sha256sum -c`, then run it with the same flags. RUSTUP_INIT_SHA256 must be the
# SHA-256 of the rustup-init binary for RUSTUP_VERSION on RUSTUP_ARCH, published at
#   https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/${RUSTUP_ARCH}/rustup-init.sha256
# NOTE(sec #45): pinned to the official digest for rustup 1.27.1 / x86_64-unknown-linux-gnu.
#   On a RUSTUP_VERSION/RUSTUP_ARCH bump, re-pin from the .sha256 URL above (build fails closed otherwise).
ARG RUSTUP_VERSION=1.27.1
ARG RUSTUP_ARCH=x86_64-unknown-linux-gnu
ARG RUSTUP_INIT_SHA256=6aeece6993e902708983b209d04c0d1dbb14ebb405ddb87def578d41f920f56d
RUN curl -fsSL -o /tmp/rustup-init \
      "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/${RUSTUP_ARCH}/rustup-init" \
 && echo "${RUSTUP_INIT_SHA256}  /tmp/rustup-init" | sha256sum -c - \
 && chmod +x /tmp/rustup-init \
 && /tmp/rustup-init -y --no-modify-path \
      --default-toolchain "${RUST_VERSION}" --profile minimal \
      --component rustfmt --component clippy \
 && rm -f /tmp/rustup-init \
 && rustc --version && cargo --version \
 && chmod -R a+rX "$RUSTUP_HOME" "$CARGO_HOME"

# 4) cargo-nextest (the project's test runner) — the official prebuilt static binary, no compile.
#    Pinned to a specific version so the image is reproducible (a bare `latest` URL would drift
#    between rebuilds). Bump NEXTEST_VERSION deliberately.
ARG NEXTEST_VERSION=0.9.140
# #33: the tarball was piped straight from the network into tar with no integrity check. Download
# it to a file, verify it against a pinned SHA-256 (fail closed) via `sha256sum -c`, then extract.
# NOTE(sec #33): pinned to the SHA-256 of https://get.nexte.st/0.9.140/linux.
#   On a NEXTEST_VERSION bump, re-pin (build fails closed via `sha256sum -c` otherwise).
ARG NEXTEST_SHA256=4ee9aaa0d0171a985a5d0eb735b87355894c1c455972e9674fb9fdbd1387c9a3
RUN curl -fsSL -o /tmp/cargo-nextest.tar.gz "https://get.nexte.st/${NEXTEST_VERSION}/linux" \
 && echo "${NEXTEST_SHA256}  /tmp/cargo-nextest.tar.gz" | sha256sum -c - \
 && tar -xzf /tmp/cargo-nextest.tar.gz -C /usr/local/cargo/bin \
 && rm -f /tmp/cargo-nextest.tar.gz \
 && chmod a+rx /usr/local/cargo/bin/cargo-nextest \
 && cargo-nextest --version

# 5) Work around the DOOM smoke tools' hardcoded absolute macOS WAD path
#    (/Users/lukebaggett/.../doom-reference/doom1.wad) so they can run unmodified: point that
#    path at the bind-mounted repo's doom-reference/ dir. See docker/README.md ("Findings").
RUN mkdir -p /Users/lukebaggett/Documents/bet \
 && ln -s /work/doom-reference /Users/lukebaggett/Documents/bet/doom-reference

WORKDIR /work
