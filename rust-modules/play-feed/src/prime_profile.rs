//! Fork Rofocale11: the old fork's four-profile, rate-gated startup prime.
//!
//! The old fork held the first Play until the stream had both enough playable media AND a pipe
//! filling faster than playback consumes it. A fixed depth alone is not proof: a 3 s buffer
//! filling at 0.4x drains before a cold torrent's swarm ramps, which is the first-seconds stutter
//! this replaces. Ported here as pure arithmetic plus a rate sampler so every decision is host
//! testable; the one upstream touch is a marked `gate` term in `native_executor::try_prime`.
//!
//! The demuxer already publishes its media-time tail for BOTH progressive and HLS streams
//! (`ff.rs`'s `VideoTailNs`/`AudioTailNs`), so the rate reading exists for an ordinary Nuvio
//! stream too — no new publisher is needed. All four profiles are decided once per session from
//! the route's [`PrimePlan`] and the armed first seek, so a reload cannot silently take a
//! different profile mid-session.

use crate::shared::PhysicalShared;
use plx_playback_contract::prime_plan::PrimePlan;
use std::sync::atomic::Ordering;

/// Fresh foreground Load. Depth alone is not enough: the pipe must also outrun the file.
pub const INITIAL_PRIME_NS: i64 = 3_000_000_000;
pub const INITIAL_PRIME_AUDIO_NS: i64 = 2_000_000_000;
/// Cold resume at a saved offset: the swarm is warm but the pieces around the offset are not.
pub const RESUME_PRIME_NS: i64 = 2_000_000_000;
pub const RESUME_PRIME_AUDIO_NS: i64 = 1_000_000_000;
/// How long each profile may hold before Play. At the ceiling the floors drop to the seek floors
/// so a source that never ramps still puts a picture on the panel instead of a black screen.
pub const INITIAL_PRIME_BUDGET_MS: u32 = 12_000;
pub const RESUME_PRIME_BUDGET_MS: u32 = 8_000;
pub const SEEK_PRIME_BUDGET_MS: u32 = 3_000;
/// Media milliseconds queued per wall second that count as keeping up (20 % headroom). Scaled by
/// the session's playback speed at the decision.
pub const PRIME_RATE_OK_MILLIS_PER_S: i64 = 1_200;
/// How often the main thread samples the demuxer's media-time counter.
pub const PRIME_RATE_SAMPLE_MS: u32 = 250;
/// A playable buffer this deep is proof on its own: on a high-bitrate file the app queue fills
/// and the media-time counter stops advancing, so the rate reading is blind while the viewer is
/// actually well supplied.
pub const PRIME_RATE_BLIND_DEPTH_NS: i64 = 8_000_000_000;
/// A preview's escape from waiting on an audio lane that never accepts.
pub const PREVIEW_AUDIO_WAIT_NS: i64 = 500_000_000;

/// How deep this session's lanes must fill before its first Play. Decided once at Engine
/// construction; a preview keeps its profile across a reload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrimeProfile {
    /// A seek, reload or in-place rebase: the original reopen floors, rate-gated with a 3 s
    /// ceiling so scrubbing stays responsive.
    Seek,
    /// A fresh foreground Load: 3 s video / 2 s audio, a rate proof, and a 12 s ceiling.
    Initial,
    /// A cold resume: 2 s / 1 s, a rate proof, and an 8 s ceiling.
    Resume,
    /// A hero trailer: the first accepted video AU (and an audio AU when the stream has one).
    Preview,
}

impl PrimeProfile {
    /// `preview` wins over both other flags: a preview that reloads later in the trailer must not
    /// start waiting on a film's buffer. A resume is told from a seek by its still-unpresented
    /// target, which is richer than the armed seek alone.
    pub fn for_session(plan: PrimePlan, rebase_pending: bool) -> Self {
        if plan.preview {
            Self::Preview
        } else if plan.resume {
            Self::Resume
        } else if plan.seek || rebase_pending {
            Self::Seek
        } else {
            Self::Initial
        }
    }

    /// `(video floor, audio floor, wall ceiling, rate-gated?)` for this profile. One table, so a
    /// session cannot answer "how much buffer" and "how long may I wait" from two places.
    pub(crate) fn policy(self) -> (i64, i64, u32, bool) {
        // The seek floors are the feed's existing prime thresholds, imported rather than
        // duplicated so an upstream change to decode latency cannot desync this table.
        use crate::native_executor::{PRIME_AUDIO_NS, PRIME_NS};
        match self {
            Self::Preview => (0, 0, 0, false),
            Self::Initial => (
                INITIAL_PRIME_NS,
                INITIAL_PRIME_AUDIO_NS,
                INITIAL_PRIME_BUDGET_MS,
                true,
            ),
            Self::Resume => (
                RESUME_PRIME_NS,
                RESUME_PRIME_AUDIO_NS,
                RESUME_PRIME_BUDGET_MS,
                true,
            ),
            Self::Seek => (PRIME_NS, PRIME_AUDIO_NS, SEEK_PRIME_BUDGET_MS, true),
        }
    }
}

/// The only facts the startup gate may look at, collected in one place so the decision is a pure
/// function and can be reproduced on the host without a television.
#[derive(Clone, Copy, Debug)]
pub struct PrimeFacts {
    /// Playable (accepted + queued) video depth, in nanoseconds of media.
    pub video_ns: i64,
    /// Playable audio depth.
    pub audio_ns: i64,
    /// Smoothed fill rate, media ms per wall second. `i64::MIN` means there is no reading yet.
    pub rate_millis_per_s: i64,
    /// Wall milliseconds since this session's prime began.
    pub wall_ms: u32,
    pub since_ms: u32,
    /// The speed this session plays at; 2x needs a 2x faster pipe.
    pub speed_permille: i32,
    /// False for segmented HLS, whose controller owns the start decision.
    pub rate_gate_enabled: bool,
    /// False when the demuxer proved the file has no audio.
    pub audio_expected: bool,
}

/// The one startup decision: may Play be released?
///
/// Two escapes keep the gate honest: a buffer deep enough to be proof on its own, and the wall
/// ceiling (a source that never ramps must not hold a black screen forever). Both lanes are
/// required unless the demuxer proved the file audioless.
pub(crate) fn prime_gate_ready(profile: PrimeProfile, f: PrimeFacts) -> bool {
    let (floor_v, floor_a, ceiling_ms, rate_gated) = profile.policy();

    // A preview's whole contract is one accepted frame; no rate, no depth, no patience.
    if !rate_gated {
        return f.video_ns > 0;
    }

    // Fork Rofocale11: the deeper floors are justified by the rate proof. Where there is no rate
    // controller — segmented HLS keeps its own runway certificate, and rigs have no demux tail —
    // the accepted-data floors are the start decision, exactly as before this port. Applying a
    // film's 3 s floor to a reload whose seek target was already consumed stalls the scrub.
    let (floor_v, floor_a) = if f.rate_gate_enabled {
        (floor_v, floor_a)
    } else {
        use crate::native_executor::{PRIME_AUDIO_NS, PRIME_NS};
        (PRIME_NS, PRIME_AUDIO_NS)
    };

    // Wrapping, not saturating: the process clock can be behind a stamped `since_ms` when a test
    // backdates it, and on a long-running TV the u32 itself wraps.
    let expired = f.wall_ms.wrapping_sub(f.since_ms) >= ceiling_ms;
    let (video_target, audio_target) = if expired {
        use crate::native_executor::{PRIME_AUDIO_NS, PRIME_NS};
        (PRIME_NS, PRIME_AUDIO_NS)
    } else {
        (floor_v, floor_a)
    };
    if f.video_ns < video_target || (f.audio_expected && f.audio_ns < audio_target) {
        return false;
    }

    if !f.rate_gate_enabled || expired || f.video_ns >= PRIME_RATE_BLIND_DEPTH_NS {
        return true;
    }
    if f.rate_millis_per_s == i64::MIN {
        // No sample yet is not a fast pipe. Wait for the first 250 ms observation.
        return false;
    }
    let speed = f.speed_permille.clamp(100, 4_000);
    let needed = PRIME_RATE_OK_MILLIS_PER_S * i64::from(speed) / 1_000;
    f.rate_millis_per_s >= needed
}

/// Smoothed fill-rate sampler over the demuxer's published media-time counter.
///
/// A non-advancing counter is a fact (the app queue is full, or the source stalled) and is
/// recorded as zero, never as "no sample" — otherwise an earlier good EMA could keep the gate
/// open while the pipe is stalled.
#[derive(Clone, Copy, Debug)]
pub struct PrimeRate {
    last_ms: u32,
    last_media_ns: i64,
    samples: u8,
    millis_per_s: i64,
}

impl Default for PrimeRate {
    fn default() -> Self {
        Self::new()
    }
}

impl PrimeRate {
    /// `i64::MIN` is load-bearing: it means "no reading yet", which blocks the gate until the
    /// first real sample rather than releasing it on a zero default.
    pub fn new() -> Self {
        Self {
            last_ms: 0,
            last_media_ns: -1,
            samples: 0,
            millis_per_s: i64::MIN,
        }
    }

    /// Forget every sample taken on the previous content timeline. The counter is absolute media
    /// time, so a seek makes it jump by minutes; a delta across that boundary is either a
    /// fictitious gigabit rate or a fictitious stall.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn millis_per_s(&self) -> i64 {
        self.millis_per_s
    }

    /// Read the pair of published tails. Video wins when present; audio is the fallback for an
    /// audioless start. `-1` on both means the lane has not produced an AU in this timeline.
    pub fn observe(&mut self, now_ms: u32, shared: &PhysicalShared) {
        let video = shared.hls_video_tail_ns.load(Ordering::Acquire);
        let audio = shared.hls_audio_tail_ns.load(Ordering::Acquire);
        let media_ns = if video >= 0 { video } else { audio };
        self.observe_media(now_ms, media_ns);
    }

    fn observe_media(&mut self, now_ms: u32, media_ns: i64) {
        if media_ns < 0 {
            self.reset();
            return;
        }
        if self.last_ms == 0 {
            self.last_ms = now_ms;
            self.last_media_ns = media_ns;
            return;
        }
        let dt = now_ms.wrapping_sub(self.last_ms);
        if dt < PRIME_RATE_SAMPLE_MS {
            return;
        }
        let delta = media_ns.saturating_sub(self.last_media_ns);
        self.last_ms = now_ms;
        self.last_media_ns = media_ns;
        self.samples = self.samples.saturating_add(1);
        if delta <= 0 {
            self.millis_per_s = 0;
            return;
        }
        let sample = (delta / 1_000_000)
            .saturating_mul(1_000)
            .checked_div(i64::from(dt))
            .unwrap_or(0);
        self.millis_per_s = if self.samples == 1 {
            sample
        } else {
            self.millis_per_s
                .saturating_mul(3)
                .saturating_add(sample)
                / 4
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_playback_contract::prime_plan::PrimePlan;

    fn facts(profile: PrimeProfile, video_s: i64, audio_s: i64, rate: i64, wall_ms: u32) -> PrimeFacts {
        let (_, _, ceiling, _) = profile.policy();
        PrimeFacts {
            video_ns: video_s * 1_000_000_000,
            audio_ns: audio_s * 1_000_000_000,
            rate_millis_per_s: rate,
            wall_ms,
            since_ms: wall_ms.saturating_sub(ceiling / 2),
            speed_permille: 1_000,
            rate_gate_enabled: true,
            audio_expected: true,
        }
    }

    #[test]
    fn profiles_are_chosen_once() {
        assert_eq!(PrimeProfile::for_session(PrimePlan::default(), false), PrimeProfile::Initial);
        assert_eq!(PrimeProfile::for_session(PrimePlan::default(), true), PrimeProfile::Seek);
        assert_eq!(
            PrimeProfile::for_session(PrimePlan { seek: true, ..Default::default() }, false),
            PrimeProfile::Seek
        );
        assert_eq!(
            PrimeProfile::for_session(PrimePlan { resume: true, ..Default::default() }, true),
            PrimeProfile::Resume
        );
        // A preview that reloads stays a preview.
        assert_eq!(
            PrimeProfile::for_session(PrimePlan { preview: true, resume: true, ..Default::default() }, true),
            PrimeProfile::Preview
        );
    }

    #[test]
    fn initial_needs_three_seconds_and_a_fast_pipe() {
        let p = PrimeProfile::Initial;
        assert!(!prime_gate_ready(p, facts(p, 3, 2, i64::MIN, 6_000)), "no rate reading yet");
        let mut f = facts(p, 3, 2, 1_200, 6_000);
        f.rate_millis_per_s = 1_199;
        assert!(!prime_gate_ready(p, f));
        let mut f = facts(p, 3, 2, 1_200, 6_000);
        f.rate_millis_per_s = 1_200;
        assert!(prime_gate_ready(p, f));
        assert!(!prime_gate_ready(p, facts(p, 2, 2, 2_000, 6_000)), "2 s is not the initial floor");
    }

    #[test]
    fn resume_and_seek_have_their_own_floors_and_ceilings() {
        assert!(prime_gate_ready(
            PrimeProfile::Resume,
            facts(PrimeProfile::Resume, 2, 1, 1_200, 4_000)
        ));
        assert!(!prime_gate_ready(
            PrimeProfile::Resume,
            facts(PrimeProfile::Resume, 1, 1, 1_200, 4_000)
        ), "1 s of video is below the resume floor");
        assert!(prime_gate_ready(
            PrimeProfile::Seek,
            facts(PrimeProfile::Seek, 1, 1, 1_200, 1_500)
        ));
        assert!(!prime_gate_ready(
            PrimeProfile::Seek,
            facts(PrimeProfile::Seek, 0, 1, 1_200, 1_500)
        ));
        // At the ceiling the floors drop to the seek floors and the rate check is skipped.
        let mut f = facts(PrimeProfile::Initial, 2, 2, i64::MIN, 13_000);
        f.since_ms = 0;
        assert!(prime_gate_ready(PrimeProfile::Initial, f));
    }

    #[test]
    fn a_deep_buffer_is_proof_and_a_slow_pipe_is_not() {
        let p = PrimeProfile::Initial;
        let mut f = facts(p, 9, 9, 200, 6_000);
        assert!(prime_gate_ready(p, f), "8 s of playable media is blind to the rate counter");
        f.video_ns = 9 * 1_000_000_000;
        f.audio_ns = 0;
        assert!(!prime_gate_ready(p, f), "an A/V file cannot start on an empty audio lane");
        f.audio_expected = false;
        assert!(prime_gate_ready(p, f));
    }

    #[test]
    fn without_a_rate_controller_the_accepted_floors_apply() {
        let p = PrimeProfile::Initial;
        let mut f = facts(p, 1, 1, i64::MIN, 6_000);
        f.rate_gate_enabled = false;
        assert!(prime_gate_ready(p, f), "HLS/rigs keep the accepted-data floors, not the film's 3 s");
        f.video_ns = 0;
        assert!(!prime_gate_ready(p, f));
    }

    #[test]
    fn preview_needs_one_frame_and_nothing_else() {
        let p = PrimeProfile::Preview;
        let mut f = facts(p, 0, 0, i64::MIN, 0);
        f.video_ns = 1;
        assert!(prime_gate_ready(p, f));
        f.video_ns = 0;
        assert!(!prime_gate_ready(p, f));
    }

    #[test]
    fn rate_sampler_smooths_and_stops_on_a_stall() {
        let mut r = PrimeRate::new();
        assert_eq!(r.millis_per_s(), i64::MIN);
        r.observe_media(1_000, 0); // baseline
        r.observe_media(1_300, 300_000_000); // 300 ms of media in 300 ms
        assert_eq!(r.millis_per_s(), 1_000);
        r.observe_media(1_600, 600_000_000);
        assert_eq!(r.millis_per_s(), 1_000);
        r.observe_media(1_900, 600_000_000); // counter stopped: a stall, not "no sample"
        assert_eq!(r.millis_per_s(), 0);
        r.reset();
        assert_eq!(r.millis_per_s(), i64::MIN);
    }
}
