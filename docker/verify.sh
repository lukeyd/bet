#!/usr/bin/env bash
# docker/verify.sh — build the `bet` toolchain and the DOOM port inside the container and run
# every self-contained check, on Ubuntu 22.04 / x86_64. Meant to be run *inside* the builder
# (see docker/README.md). Continue-on-error: every stage runs, a summary prints at the end, and
# the script exits non-zero if any stage failed.
#
# What is NOT here: the full doomgeneric differential oracle. Its inputs (goldens/oracle.patch
# and goldens/*.oracle.sync) are gitignored id-GPL-derived artifacts, so tic-by-tic parity vs
# id's engine is not reproducible from a clean clone. We verify everything that is: the workspace
# gate, the LLVM backend, that 61k lines of DOOM compile to a native x86_64 binary, that it runs
# real demos headless to completion, and the committed oracle-independent golden fingerprints.
set -uo pipefail
cd "$(git rev-parse --show-toplevel 2>/dev/null || echo /work)"

FAILED=0
declare -a RESULTS
sec()  { printf '\n\033[1;36m========== %s ==========\033[0m\n' "$*"; }
ok()   { RESULTS+=("PASS  $1"); printf '\033[1;32m  PASS\033[0m  %s\n' "$1"; }
bad()  { RESULTS+=("FAIL  $1"); printf '\033[1;31m  FAIL\033[0m  %s\n' "$1"; FAILED=1; }
step() { local name="$1"; shift; if "$@"; then ok "$name"; else bad "$name"; fi; }

sec "toolchain"
echo "arch:  $(uname -m)   os: $(. /etc/os-release; echo "$PRETTY_NAME")"
rustc --version; cargo --version
echo "llvm:  $(llvm-config-18 --version)   prefix: ${LLVM_SYS_180_PREFIX:-<unset>}"
cc --version | head -1
# Report the container's cgroup memory cap (set by compose.yaml's mem_limit) so an OOM in a
# stage below is unambiguous rather than mysterious.
memmax="$(cat /sys/fs/cgroup/memory.max 2>/dev/null || cat /sys/fs/cgroup/memory/memory.limit_in_bytes 2>/dev/null || echo max)"
if [[ "$memmax" =~ ^[0-9]+$ ]]; then
  echo "cgroup memory cap: $((memmax / 1024 / 1024 / 1024)) GiB (set BET_MEM_LIMIT to change)"
else
  echo "cgroup memory cap: $memmax (no limit — set BET_MEM_LIMIT / compose mem_limit to protect the host)"
fi

# ---------------------------------------------------------------------------
sec "0. shareware doom1.wad"
# The shareware WAD is freely redistributable; its DEMO1/2/3 lumps drive the demo playback.
WAD_DIR="doom-reference"; WAD="$WAD_DIR/doom1.wad"
WAD_MD5_GOOD="f0cefca49926d00903cf57551d901abe"   # canonical v1.9 shareware doom1.wad
WAD_URLS=(
  "https://distro.ibiblio.org/slitaz/sources/packages/d/doom1.wad"
  "https://github.com/Akbar30Bill/DOOM_wads/raw/master/doom1.wad"
)
mkdir -p "$WAD_DIR"
if [[ ! -f "$WAD" ]]; then
  for u in "${WAD_URLS[@]}"; do
    echo ">> fetching $u"
    if curl -fsSL --retry 2 -o "$WAD" "$u"; then break; fi
  done
fi
if [[ -f "$WAD" ]]; then
  got="$(md5sum "$WAD" | cut -d' ' -f1)"
  echo "doom1.wad: $(stat -c %s "$WAD") bytes  md5=$got"
  if [[ "$got" == "$WAD_MD5_GOOD" ]]; then ok "WAD present (canonical v1.9 shareware)"
  else bad "WAD md5 mismatch (got $got, want $WAD_MD5_GOOD) — demo/golden checks may diverge"; fi
else
  bad "WAD download failed — drop a shareware doom1.wad in $WAD_DIR/ and re-run"
fi

# ---------------------------------------------------------------------------
sec "1. default workspace gate (no LLVM) — frontend / runtime / interp / tooling"
step "fmt"         cargo fmt --all --check
step "clippy"      cargo clippy --workspace --all-targets -- -D warnings
step "graph-check" cargo xtask graph-check
step "build"       cargo build --workspace
step "nextest"     cargo nextest run --workspace --no-tests=pass

# ---------------------------------------------------------------------------
sec "2. LLVM 18 backend builds (--features llvm)"
step "driver+llvm" cargo build -p driver --features llvm
step "runtime"     cargo build -p runtime      # headless real runtime (byte-identical to gg-desktop+HEADLESS)

# ---------------------------------------------------------------------------
sec "3. compile the DOOM port -> native x86_64 binary"
# NOTE: this stage currently drives the `bet` backend into runaway memory (>45 GB). The
# container's cgroup cap (above) turns that into a clean in-container OOM (exit 137) instead of a
# host-endangering event. Exit 137 is reported distinctly so it reads as "hit the memory cap".
target/debug/bet build ports/doom/doom.bet --runtime real -o /tmp/doom
rc=$?
if [[ $rc -eq 0 ]]; then
  ok "compile ports/doom/doom.bet"
  file /tmp/doom; ls -la /tmp/doom
elif [[ $rc -eq 137 || $rc -eq 139 ]]; then
  bad "compile ports/doom/doom.bet (exit $rc — OOM-killed at the cgroup cap; runaway memory in the bet backend)"
else
  bad "compile ports/doom/doom.bet (exit $rc)"
fi

# ---------------------------------------------------------------------------
sec "4. run the port headless on the shareware WAD (real demo playback)"
if [[ -x /tmp/doom && -f "$WAD" ]]; then
  for d in demo1 demo2 demo3; do
    if BET_GG_HEADLESS=1 /tmp/doom -iwad "$WAD" -timedemo "$d" -sync "/tmp/$d.sync" >/tmp/$d.log 2>&1; then
      tics="$(grep -c '^T=' "/tmp/$d.sync" 2>/dev/null || echo 0)"
      ok "$d headless playback ($tics fingerprint tics)"
      head -1 "/tmp/$d.sync" 2>/dev/null || true
    else
      bad "$d headless playback (see /tmp/$d.log)"; tail -5 "/tmp/$d.log" 2>/dev/null || true
    fi
  done
else
  bad "skipped demo playback (no binary or no WAD)"
fi

# ---------------------------------------------------------------------------
sec "5. oracle-independent golden fingerprints (native smoke -> committed *.golden)"
# Each tool is a native headless program that loads the WAD and prints deterministic CRCs; the
# committed golden is its frozen output. basename(tool)_smoke <-> <golden>.golden.
declare -A GOLD=(
  [rdata_smoke]=rdata
  [renderworld_smoke]=renderworld
  [renderthings_smoke]=renderthings
  [simcore_smoke]=simcore
  [simmove_smoke]=simmove
  [statusbar_smoke]=statusbar
  [saveg_smoke]=saveg
)
if [[ -x /tmp/doom ]]; then
  for tool in "${!GOLD[@]}"; do
    golden="ports/doom/goldens/${GOLD[$tool]}.golden"
    src="ports/doom/tools/${tool}.bet"
    [[ -f "$src" && -f "$golden" ]] || { bad "golden:$tool (missing src or golden)"; continue; }
    if ! target/debug/bet build "$src" --runtime real -o "/tmp/$tool" >/tmp/$tool.build.log 2>&1; then
      bad "golden:$tool (compile failed; /tmp/$tool.build.log)"; continue
    fi
    BET_GG_HEADLESS=1 "/tmp/$tool" >"/tmp/$tool.out" 2>/tmp/$tool.run.log || true
    if diff -q "/tmp/$tool.out" "$golden" >/dev/null 2>&1; then
      ok "golden:$tool matches ${GOLD[$tool]}.golden"
    else
      bad "golden:$tool differs from ${GOLD[$tool]}.golden"
      diff "$golden" "/tmp/$tool.out" | head -8 || true
    fi
  done
else
  bad "skipped golden smokes (no compiler binary)"
fi

# ---------------------------------------------------------------------------
sec "SUMMARY (Ubuntu 22.04 / x86_64)"
printf '%s\n' "${RESULTS[@]}"
if [[ "$FAILED" -eq 0 ]]; then
  printf '\n\033[1;32mALL CHECKS PASSED\033[0m — the bet toolchain + DOOM port build and verify on Linux/x86_64.\n'
else
  printf '\n\033[1;31mSOME CHECKS FAILED\033[0m — see stages above.\n'
fi
exit "$FAILED"
