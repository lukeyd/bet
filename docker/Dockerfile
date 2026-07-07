# Reproducible build + verify environment for the `bet` toolchain and the DOOM port.
#
# Pins the second platform the project has never been exercised on: Ubuntu 22.04 / x86_64
# (the author built everything on macOS/ARM64). Gives a sudo-free, disposable environment with
# Rust (pinned by rust-toolchain.toml) + LLVM 18 (the `backend --features llvm` dependency).
#
# The image contains NO project source — the repo is bind-mounted at /work at run time (see
# compose.yaml), so a code change never means an image rebuild.
FROM ubuntu:22.04

SHELL ["/bin/bash", "-o", "pipefail", "-c"]
ENV DEBIAN_FRONTEND=noninteractive

# 1) Base build tooling + the system libraries LLVM's static libs (linked by llvm-sys) and the
#    final compiled programs reference. The `gg-desktop` block (alsa/x11/wayland/xkbcommon) is
#    only needed to *build* the windowed runtime (minifb + cpal); the headless correctness path
#    does not use it, but installing it keeps the desktop build available too.
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl gnupg git build-essential pkg-config cmake xz-utils \
      zlib1g-dev libzstd-dev libffi-dev libxml2-dev libedit-dev libncurses5-dev \
      libasound2-dev libx11-dev libxkbcommon-dev libwayland-dev \
 && rm -rf /var/lib/apt/lists/*

# 2) LLVM 18 from the official apt.llvm.org repo (jammy). Provides llvm-config-18 and the static
#    archives that llvm-sys 180 links against, plus clang/lld. Its own layer so tweaking the
#    app-dependency list above never re-downloads ~half a gigabyte of LLVM.
RUN curl -fsSL https://apt.llvm.org/llvm-snapshot.gpg.key | gpg --dearmor -o /usr/share/keyrings/llvm.gpg \
 && echo "deb [signed-by=/usr/share/keyrings/llvm.gpg] http://apt.llvm.org/jammy/ llvm-toolchain-jammy-18 main" \
      > /etc/apt/sources.list.d/llvm18.list \
 && apt-get update \
 && apt-get install -y --no-install-recommends \
      llvm-18 llvm-18-dev llvm-18-tools libpolly-18-dev clang-18 lld-18 \
 && rm -rf /var/lib/apt/lists/*

# 3) Rust, pinned to the repo toolchain. Installed under /usr/local so any UID can use it; the
#    trees are made world-writable so a non-root runtime user (see `user:` in compose.yaml) can
#    populate the cargo registry cache. RUSTUP_HOME/CARGO_HOME/PATH are baked in as ENV so the
#    proxies resolve regardless of who runs them.
ARG RUST_VERSION=1.96.1
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    LLVM_SYS_180_PREFIX=/usr/lib/llvm-18 \
    PATH=/usr/local/cargo/bin:/usr/lib/llvm-18/bin:/usr/local/bin:/usr/bin:/bin
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --no-modify-path \
      --default-toolchain "${RUST_VERSION}" --profile minimal \
      --component rustfmt --component clippy \
 && rustc --version && cargo --version \
 && chmod -R a+rwX "$RUSTUP_HOME" "$CARGO_HOME"

# 4) cargo-nextest (the project's test runner) — the official prebuilt static binary, no compile.
#    Pinned to a specific version so the image is reproducible (a bare `latest` URL would drift
#    between rebuilds). Bump NEXTEST_VERSION deliberately.
ARG NEXTEST_VERSION=0.9.140
RUN curl -fsSL "https://get.nexte.st/${NEXTEST_VERSION}/linux" | tar -xzf - -C /usr/local/cargo/bin \
 && chmod a+rx /usr/local/cargo/bin/cargo-nextest \
 && cargo-nextest --version

# 5) Work around the DOOM smoke tools' hardcoded absolute macOS WAD path
#    (/Users/lukebaggett/.../doom-reference/doom1.wad) so they can run unmodified: point that
#    path at the bind-mounted repo's doom-reference/ dir. See docker/README.md ("Findings").
RUN mkdir -p /Users/lukebaggett/Documents/bet \
 && ln -s /work/doom-reference /Users/lukebaggett/Documents/bet/doom-reference

WORKDIR /work
