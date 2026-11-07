#!/bin/bash
# Reproduce — on the HOST, in about two seconds — the crash that killed the app ~25 minutes into
# a 4K remux: a SIGSEGV inside `libtorrent::peer_connection::reject_piece`, whose `lr` symbolized
# to offset 0x390898 of `libsbengine.so`, immediately after `bl memmove@plt`.
#
# # Why a host check and not the television
#
# The field crash needs a 26 GB torrent, 33 peers and a collapsing swarm, i.e. a 45-minute viewing
# session per attempt — the owner said plainly that this is not affordable. It does not need any of
# that: the fault is in the ERASE LOOP, not in the swarm. `m_requests` is a `std::vector` of
# 12-byte `peer_request` (peer_request.hpp:43), the loop caches `m_requests.end()` before it starts
# erasing, and `i = erase(i)` plus the loop's `++i` steps the iterator past the SHRUNK end. On the
# NDK's libstdc++ the length then underflows and `erase` memmoves ~4 GB: `src` was `0xd94fffe0`,
# 0x20 bytes below the unmapped page at `0xd9500000`, which is where the television died.
#
# This check compiles `reject_piece` VERBATIM out of the pinned tree
# (`build/sbengine/deps/nuvio_libtorrent-src/src/peer_connection.cpp`) into a stand-in class that
# declares only the members the function touches. Nothing here re-describes the loop, so it cannot
# drift from what ships: fix the source and this passes, break it again and this fails.
#
# The pinned tree is gitignored and absent on a clean clone, so this is not a `make check` step —
# run it after `ci/build-sbengine.sh` has populated the deps (or `tools/host-engine-check.sh`),
# before and after changing the patch series.
#
# It is built with ASan+UBSan, but those are a precaution, not the detector: measured on this host,
# the past-the-end `erase` does NOT fault. GCC 16's libstdc++ copies only a positive distance
# (`bits/stl_algobase.h:421-422`), so the loop only leaves the queue wrong — which is what the
# assertions below grade. The NDK's libstdc++ 12.2.0 does the copy (`bits/stl_algobase.h:429-431`,
# `if (_Num) __builtin_memmove(..., sizeof(_Tp) * _Num)`), and a negative length there is the
# television's SIGSEGV. Same undefined behaviour, two toolchains: grade the invariant, not the
# symptom.
#
# Usage: tools/engine-reject-piece-check.sh
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="${PLX_LIBTORRENT_SRC:-$REPO_ROOT/build/sbengine/deps/nuvio_libtorrent-src}"
FUNCTION="$SRC/src/peer_connection.cpp"

if [ ! -f "$FUNCTION" ]; then
    echo "engine-reject-piece-check: no pinned libtorrent at $SRC — build the engine first" >&2
    echo "  (ci/build-sbengine.sh or tools/host-engine-check.sh); nothing to check" >&2
    exit 0
fi

OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT

# The function, from its signature to its own closing brace. Both the buggy and the fixed shape
# keep that closing brace as the first line that is one tab and a `}`.
awk '/void peer_connection::reject_piece/ { on = 1 } on { print } on && /^\t}$/ { exit }' \
    "$FUNCTION" > "$OUT/reject_piece_body.inc"
if ! grep -q 'm_requests' "$OUT/reject_piece_body.inc"; then
    echo "engine-reject-piece-check: could not extract reject_piece from $FUNCTION" >&2
    exit 2
fi

# Say which shape is about to run, so a pass/fail is never mistaken for the other tree.
if grep -q 'for (auto i = m_requests.begin(); i != m_requests.end();)' "$OUT/reject_piece_body.inc"; then
    echo "engine-reject-piece-check: loop re-reads m_requests.end() (patched)"
else
    echo "engine-reject-piece-check: loop caches a stale end iterator (the crash)"
fi

# `config.hpp` pulls `boost/config.hpp`; the engine build already staged the pinned Boost beside
# the libtorrent clone it belongs to.
BOOST_SRC="${PLX_BOOST_SRC:-$REPO_ROOT/build/sbengine/deps/nuvio_boost-src}"
[ -f "$BOOST_SRC/boost/config.hpp" ] || { echo "engine-reject-piece-check: no Boost headers at $BOOST_SRC" >&2; exit 0; }

CXX="${CXX:-g++}"
"$CXX" -std=c++20 -O1 -g -Wall -Wextra -Werror -fsanitize=address,undefined -fno-omit-frame-pointer \
    -I"$SRC/include" -I"$BOOST_SRC" -I"$OUT" \
    "$REPO_ROOT/tools/engine_reject_piece_check.cpp" -o "$OUT/check"
ASAN_OPTIONS=detect_leaks=0 "$OUT/check"
