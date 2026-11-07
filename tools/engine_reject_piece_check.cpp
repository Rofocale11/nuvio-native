/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under GPL-3.0-or-later; see LICENSE at the repository root.
 *
 * engine_reject_piece_check.cpp — the harness half of `tools/engine-reject-piece-check.sh`.
 *
 * The function under test is NOT written here. The script extracts
 * `peer_connection::reject_piece` verbatim from the pinned libtorrent tree and puts it in
 * `reject_piece_body.inc`, which this file includes at namespace scope after a stand-in
 * `peer_connection` declaring the members that function touches. So the loop that runs is the
 * loop that ships, and a patch that fixes it makes this pass without an edit here.
 *
 * Everything is compiled with ASan+UBSan, but they are a precaution, not the detector — measured
 * on this host, the bug does not fault at all. GCC 16's libstdc++ copies only when the distance is
 * positive (`bits/stl_algobase.h:421-422`, `if (__builtin_expect(__n > 1, true))`), so the same
 * past-the-end `std::vector::erase` copies nothing and silently corrupts the queue instead. The
 * NDK's libstdc++ 12.2.0 has no such guard: `bits/stl_algobase.h:429-431` is
 * `const ptrdiff_t _Num = __last - __first; if (_Num) __builtin_memmove(__result, __first,
 * sizeof(_Tp) * _Num);`, so a negative `_Num` is truthy and the 12-byte elements multiply out to a
 * ~4 GB memmove — the `bl memmove@plt` at 0x390894 with `src` 0x20 below an unmapped page.
 *
 * So this grades the INVARIANT the loop must keep (a dropped piece removes exactly the requests
 * for it, and no others), which is the same undefined behaviour seen from the safe side of the
 * toolchain difference.
 */
#include <libtorrent/peer_request.hpp>
#include <cstddef>
#include <cstdio>
#include <vector>

// A no-op here; the pinned tree's own macro is compiled out in a release build anyway.
#define TORRENT_ASSERT(x) do {} while (0)

namespace libtorrent {

// Only the counter the loop touches; the real `counters` enum has hundreds.
namespace counters { enum { num_peers_up_requests }; }

struct counters_stub
{
	int decrements = 0;
	void inc_stats_counter(int, int const delta = 1) { decrements += delta; }
};

// The members `reject_piece` actually uses, public so the harness can fill the queue directly.
class peer_connection
{
public:
	std::vector<peer_request> m_requests;
	counters_stub m_counters;
	int rejected = 0;

	bool is_single_thread() const { return true; }
	void write_reject_request(peer_request const&) { ++rejected; }

	void reject_piece(piece_index_t index);
};

// The frozen function, defined inside this namespace so its unqualified names resolve as they do
// in the pinned tree. It closes the namespace below.
#include "reject_piece_body.inc"

} // namespace libtorrent

namespace {

int failures = 0;

void eq_int(char const* what, int const got, int const want)
{
	if (got == want) return;
	std::printf("FAIL %s: got %d, want %d\n", what, got, want);
	++failures;
}

libtorrent::peer_request req(int const piece, int const start)
{
	return libtorrent::peer_request{libtorrent::piece_index_t(piece), start, 16384};
}

} // namespace

int main()
{
	using libtorrent::piece_index_t;

	// The field shape: every queued request is for the piece being dropped, so the erase empties
	// the queue from under the loop. On the ARM build the third turn is the fatal `erase` — the
	// queue still holds that piece's bytes past its new end, so the guard reads them as a match
	// and the copy runs off the buffer. On this host the same turn only shrinks the queue (see the
	// header), which is why the assertion below it is the one that discriminates.
	{
		libtorrent::peer_connection pc;
		for (int k = 0; k < 3; ++k) pc.m_requests.push_back(req(7, k * 16384));
		pc.reject_piece(piece_index_t(7));
		eq_int("all-queued-for-the-dropped-piece: requests left", int(pc.m_requests.size()), 0);
		eq_int("all-queued-for-the-dropped-piece: rejects written", pc.rejected, 3);
		eq_int("all-queued-for-the-dropped-piece: up_request accounting", pc.m_counters.decrements, -1);
	}

	// The discriminator, and the realistic one: the piece sits between requests that must survive,
	// and the LAST queued request is for it — the element a peer is most likely to have outstanding
	// when the streaming window drops the piece. A cached `end` lets the loop run one turn past the
	// shrunk queue and reject an already-erased request a second time: 3 rejects where 2 were
	// queued, and a surviving request for another piece silently lost. That is an outstanding block
	// nobody is tracking any more, which is the stall/hash-fail churn the field stats showed
	// (`refet`, throughput collapsing to KB/s) even when the memmove happens to miss.
	{
		libtorrent::peer_connection pc;
		pc.m_requests.push_back(req(3, 0));
		pc.m_requests.push_back(req(7, 16384));
		pc.m_requests.push_back(req(3, 32768));
		pc.m_requests.push_back(req(7, 49152));
		pc.reject_piece(piece_index_t(7));
		eq_int("tail-of-a-mixed-queue: requests left", int(pc.m_requests.size()), 2);
		eq_int("tail-of-a-mixed-queue: rejects written", pc.rejected, 2);
		eq_int("tail-of-a-mixed-queue: kept order unchanged",
			pc.m_requests.size() == 2 && pc.m_requests[0] == req(3, 0) && pc.m_requests[1] == req(3, 32768) ? 1 : 0, 1);
	}

	// A dropped piece nobody asked for must not touch the queue at all.
	{
		libtorrent::peer_connection pc;
		pc.m_requests.push_back(req(3, 0));
		pc.reject_piece(piece_index_t(7));
		eq_int("piece-nobody-queued: requests left", int(pc.m_requests.size()), 1);
		eq_int("piece-nobody-queued: rejects written", pc.rejected, 0);
	}

	if (failures != 0)
	{
		std::printf("reject_piece: %d assertion(s) failed\n", failures);
		return 1;
	}
	std::printf("reject_piece: the erase loop keeps the queue consistent\n");
	return 0;
}
