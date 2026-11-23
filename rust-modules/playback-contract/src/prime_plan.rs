//! Fork Rofocale11: the route's three startup-prime facts, in a neutral contract type.
//!
//! The old fork's four-profile gate needs to know, at the moment the physical Load is prepared,
//! whether this is a trailer preview, whether there is an unpresented resume target, and whether
//! the stream is segmented HLS (whose own controller owns the start decision). `OpenRequest`
//! crosses the route/feed boundary and must not name route concepts, so those facts travel as this
//! fork-owned plan. The policy table itself lives in `plx_play_feed::prime_profile`.

/// What the feed needs to choose this session's startup prime profile.
///
/// `Default` is deliberately all-false: callers that do not carry a route (local samples, test
/// rigs) get the plain Initial profile with the rate gate off, which releases on depth alone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PrimePlan {
    /// A hero trailer: its first accepted AU starts the clock, with no rate check or ceiling.
    pub preview: bool,
    /// A cold resume at a saved offset: shallower floors than a fresh Load, still rate-gated.
    pub resume: bool,
    /// A reload for a seek/rebase. Its new Load must not pay a fresh play's deeper buffer gate.
    pub seek: bool,
    /// False for segmented HLS, whose controller owns the start decision.
    pub rate_gate: bool,
}
