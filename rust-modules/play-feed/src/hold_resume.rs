//! a playable-reserve release for a progressive stall hold.
//!
//! The upstream hold releases only when a queue reaches its byte cap (or the demuxer ends). On a
//! slow torrent that can mean twenty seconds of Buffering while the demuxer has already staged
//! several seconds of playable media — the byte cap is a *capacity* fact, not a time fact. This
//! module adds a bounded media-time condition: resume once the stream holds
//! [`HOLD_RESUME_NS`] of video ahead of the frozen playhead, with a small audio floor when the
//! stream has an audio lane. The byte-cap and EOF escapes stay in force: a queue parked at its cap
//! cannot grow any further, and the end of the film cannot wait for more media.
//!
//! The trade-off is deliberate: a link slower than the file will now produce several short holds
//! instead of one long one (duty `1 - delivered/required` is unchanged). The video lane is the
//! binding one for bitrate, so three seconds there is the promise; an empty audio lane must still
//! hold at least [`HOLD_RESUME_AUDIO_NS`] so Play does not start on a silent master clock.

/// Video playable reserve required to resume a progressive hold (3 seconds).
pub const HOLD_RESUME_NS: i64 = 3_000_000_000;
/// Audio reserve required as well when the stream has an audio lane (300 ms). A full three
/// seconds of audio is unnecessary: the 1 MiB audio queue holds far more media time than video.
pub const HOLD_RESUME_AUDIO_NS: i64 = 300_000_000;

/// Is there enough playable media ahead of the held playhead to resume?
///
/// Both arguments are lane tails minus the presented position, in nanoseconds of fed media time.
/// A negative reserve is not a small reserve: it means the clock has passed the tail, so answer no.
pub fn ready(video_ns: i64, audio_ns: i64, audio_expected: bool) -> bool {
    if video_ns < HOLD_RESUME_NS {
        return false;
    }
    !audio_expected || audio_ns >= HOLD_RESUME_AUDIO_NS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_seconds_of_video_resumes() {
        assert!(!ready(3_000_000_000 - 1, 0, false));
        assert!(ready(3_000_000_000, 0, false));
        assert!(ready(8_000_000_000, 0, false));
    }

    #[test]
    fn an_audio_lane_needs_its_small_floor() {
        assert!(!ready(3_000_000_000, 0, true));
        assert!(!ready(3_000_000_000, HOLD_RESUME_AUDIO_NS - 1, true));
        assert!(ready(3_000_000_000, HOLD_RESUME_AUDIO_NS, true));
    }

    #[test]
    fn a_tail_the_clock_passed_never_resumes() {
        assert!(!ready(-1, i64::MAX, true));
        assert!(!ready(i64::MAX, -1, true));
    }
}
