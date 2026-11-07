#!/bin/bash
# Build `libsbengine.so` for the HOST (x86_64) and run the host-only C-engine checks against it.
#
# The shipping library is 32-bit ARM (ci/build-sbengine.sh, webOS NDK). This script is the host
# twin: it reuses the Boost/libtorrent sources that build already populated under
# `build/sbengine/deps` (FetchContent's SOURCE_DIR overrides, so nothing is downloaded), compiles
# the same wrapper, and puts the result where `plx_stremio::sbengine` looks for it when
# `PLXNATIVE_APP_DIR` names that directory.
#
# `--check` (default) then runs three host-only checks:
#   * `pmsshim/.../host_c_engine_keeps_the_decision_silent_past_the_media_budget` — the REAL shim +
#     REAL C engine + REAL loopback socket: a peerless magnet holds `/decision` silent for the
#     engine's 30 s metadata budget and the request still gets its answer. That is the shape that
#     used to fail: the 120 s decision deadline was silently capped at the 15 s media stall budget.
#   * `pmsshim/.../host_torrent_plays_on_first_try` — a loopback seeder built from the same pinned
#     libtorrent seeds a real MP4; the shim's FIRST `/decision` for it answers directplay and the
#     Part redirect serves the seeded file's bytes through the engine.
#   * `plx_plex http::tests::a_deadline_only_request_survives_silence_the_stall_budget_cuts_short`
#     — the transport half, with a discriminating 100 ms stall.
#
# Usage: tools/host-engine-check.sh [--build-only]
set -eu
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$REPO_ROOT/build/sbengine-host"
WORK="$OUT/src"
HOST_SO="$OUT/libsbengine.so"
DEPS="$REPO_ROOT/build/sbengine/deps"
BOOST_SRC="$DEPS/nuvio_boost-src"
LT_SRC="$DEPS/nuvio_libtorrent-src"

if [ ! -d "$BOOST_SRC" ] || [ ! -d "$LT_SRC" ]; then
    echo "host-engine-check: the populated FetchContent sources are missing." >&2
    echo "  run ci/build-sbengine.sh once (it downloads Boost 1.86 and libtorrent into build/sbengine/deps)" >&2
    exit 1
fi

if [ ! -f "$HOST_SO" ]; then
    echo "host-engine-check: building the host engine and its seeder"
fi

echo "=== staging the wrapper ==="
rm -rf "$WORK"
mkdir -p "$WORK"
cp -R "$REPO_ROOT/vendor/engine-sources/nuvio-engine" "$WORK/nuvio-engine"
rm -rf "$WORK/nuvio-engine/.git" "$WORK/nuvio-engine/build"
cp "$REPO_ROOT/vendor/engine-sources/sbengine.cpp" "$WORK/"
cp "$REPO_ROOT/tools/host_engine_seed.cpp" "$WORK/host_seed.cpp"
mkdir -p "$WORK/core"
cp "$REPO_ROOT/vendor/engine-sources/core/sbengine.h" "$WORK/core/"

cat > "$WORK/CMakeLists.txt" <<'EOF'
cmake_minimum_required(VERSION 3.24)
project(sbengine_wrapper CXX)
add_subdirectory(nuvio-engine)
add_library(sbengine SHARED sbengine.cpp)
target_compile_features(sbengine PRIVATE cxx_std_20)
target_include_directories(sbengine PRIVATE
  "${CMAKE_CURRENT_SOURCE_DIR}/nuvio-engine/include"
  "${CMAKE_CURRENT_SOURCE_DIR}"
)
target_link_libraries(sbengine PRIVATE nuvio_engine)
set_target_properties(sbengine PROPERTIES
  OUTPUT_NAME "sbengine"
  CXX_VISIBILITY_PRESET hidden
  VISIBILITY_INLINES_HIDDEN ON)
target_link_options(sbengine PRIVATE "LINKER:--exclude-libs,ALL")

# The loopback seeder shares the engine's pinned libtorrent so both sides speak the same protocol.
add_executable(host_seed host_seed.cpp)
target_compile_features(host_seed PRIVATE cxx_std_20)
target_link_libraries(host_seed PRIVATE torrent-rasterbar)
EOF

echo "=== cmake configure (host) ==="
cmake -S "$WORK" -B "$OUT/build" -G "Unix Makefiles" \
  -DFETCHCONTENT_SOURCE_DIR_NUVIO_BOOST="$BOOST_SRC" \
  -DFETCHCONTENT_SOURCE_DIR_NUVIO_LIBTORRENT="$LT_SRC" \
  -DCMAKE_BUILD_TYPE=Release \
  -DNUVIO_ENGINE_ENABLE_LIBTORRENT=ON \
  -DNUVIO_ENGINE_BUILD_SHARED=OFF \
  -DNUVIO_ENGINE_BUILD_TESTS=OFF \
  -DCMAKE_POSITION_INDEPENDENT_CODE=ON

echo "=== build ==="
cmake --build "$OUT/build" --target sbengine host_seed -j"$(nproc)"
cp "$OUT/build/libsbengine.so" "$HOST_SO"

echo "=== result ==="
file "$HOST_SO"
file "$OUT/build/host_seed"

[ "${1:-}" = "--build-only" ] && exit 0

echo "=== check: the real shim + the real C engine hold /decision past the 15 s media budget ==="
cd "$REPO_ROOT/rust-modules"
# `plx_base/hostsim` is what makes `PLXNATIVE_APP_DIR` steer the app dir (see ENV_STEERABLE);
# without it the test binary looks for libsbengine.so beside itself, not in $OUT.
PATH="$HOME/.cargo/bin:$PATH" \
  CARGO_TARGET_DIR=target-fast \
  PLXNATIVE_RUNTIME_DIR="$(mktemp -d)" \
  PLXNATIVE_APP_DIR="$OUT" \
  PLX_HOST_ENGINE_CHECK=1 \
  cargo nextest run -p plx_pmsshim --features plx_base/hostsim --run-ignored only \
    --success-output final \
    host_c_engine_keeps_the_decision_silent_past_the_media_budget

echo "=== check: a seeded torrent direct-plays on the FIRST decision and serves its bytes ==="
PAYLOAD="$REPO_ROOT/site/media/feel-1080p60.h264.mp4"
SEED_LOG="$(mktemp)"
"$OUT/build/host_seed" "$PAYLOAD" >"$SEED_LOG" 2>&1 &
SEED_PID=$!
cleanup_seed() {
    status=$?
    kill "$SEED_PID" 2>/dev/null || true
    wait "$SEED_PID" 2>/dev/null || true
    if [ "$status" -ne 0 ]; then
        echo "--- seeder log ---" >&2
        grep -v 'block ' "$SEED_LOG" | tail -20 >&2
    fi
    rm -f "$SEED_LOG"
}
trap cleanup_seed EXIT
ready=
for _ in $(seq 1 200); do
    if grep -q '^READY$' "$SEED_LOG" 2>/dev/null; then ready=1; break; fi
    kill -0 "$SEED_PID" 2>/dev/null || break
    sleep 0.05
done
if [ -z "$ready" ]; then
    cat "$SEED_LOG"
    echo "host-engine-check: the seeder never became ready" >&2
    exit 1
fi
HASH="$(grep -m1 '^HASH=' "$SEED_LOG" | cut -d= -f2)"
HOST="$(grep -m1 '^HOST=' "$SEED_LOG" | cut -d= -f2)"
PORT="$(grep -m1 '^PORT=' "$SEED_LOG" | cut -d= -f2)"
echo "seeding $PAYLOAD as $HASH on $HOST:$PORT"
PATH="$HOME/.cargo/bin:$PATH" \
  CARGO_TARGET_DIR=target-fast \
  PLXNATIVE_RUNTIME_DIR="$(mktemp -d)" \
  PLXNATIVE_APP_DIR="$OUT" \
  PLX_HOST_ENGINE_CHECK=1 \
  PLX_HOST_TORRENT_MAGNET="magnet:?xt=urn:btih:$HASH&x.pe=$HOST:$PORT" \
  PLX_HOST_TORRENT_FILE="$PAYLOAD" \
  cargo nextest run -p plx_pmsshim --features plx_base/hostsim --run-ignored only \
    --success-output final \
    host_torrent_plays_on_first_try
cleanup_seed
trap - EXIT

echo "=== check: the deadline-only transport survives silence the media budget cuts short ==="
PATH="$HOME/.cargo/bin:$PATH" \
  CARGO_TARGET_DIR=target-fast \
  PLXNATIVE_RUNTIME_DIR="$(mktemp -d)" \
  cargo nextest run -p plx_plex a_deadline_only_request_survives_silence
