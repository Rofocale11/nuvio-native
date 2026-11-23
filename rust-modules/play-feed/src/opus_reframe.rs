//! # Fork Rofocale11: why this is a new file
//!
//! Multichannel Opus reaches this television as app-decoded stereo PCM. The source packets in the
//! addon files carry sparse timestamps: 20 ms of audio stamped 160 ms apart. Both `PcmConverter`'s
//! clock and `ff_pcm::AuBatcher` re-anchor on an `AU_GAP_TOLERANCE_NS` (20 ms) gap, so each 20 ms
//! decoded block inherits the 160 ms spacing — the television plays 20 ms and waits 140 ms, which
//! is the stutter. Upstream never sees this because PMS converts the track to a continuous stereo
//! stream first; this fork has no transcoder.
//!
//! This clock is independent of the source timeline. The first decoded block anchors it (at the
//! packet's pts, so the demuxer's published `FirstAudioPts` and the feed lane's first-AU logic
//! still match), and each later block is stamped at `previous + the duration of the bytes actually
//! decoded`. It is a child module of `ff` because it must be applied where the decoded bytes are,
//! but its arithmetic is pure and tested here. No other app-decoded codec takes this path.

/// An independent, monotonic AU clock for app-decoded Opus PCM.
#[derive(Debug, Default)]
pub struct OpusReframe {
    next: Option<i64>,
}

impl OpusReframe {
    pub fn new() -> Self {
        Self { next: None }
    }

    /// A seek (or any new content timeline) re-anchors at the next decoded block.
    pub fn reset(&mut self) {
        self.next = None;
    }

    /// Stamp one decoded block and advance the clock by the PCM's own duration.
    ///
    /// `anchor_pts` is the source packet's pts, used only for the first block; `bytes` is what the
    /// decoder actually produced; `rate` and `frame_bytes` describe the declared PCM output.
    pub fn stamp(&mut self, anchor_pts: i64, bytes: usize, rate: u32, frame_bytes: u32) -> i64 {
        let rate = i64::from(rate.max(1));
        let frame_bytes = i64::from(frame_bytes.max(1));
        let duration = bytes as i64 * 1_000_000_000 / (rate * frame_bytes);
        let pts = self.next.unwrap_or(anchor_pts);
        self.next = Some(pts + duration);
        pts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const FRAME_BYTES: u32 = 4; // 48 kHz 16-bit stereo
    const BLOCK: usize = 3840; // 20 ms of that PCM

    #[test]
    fn first_block_uses_the_packet_anchor() {
        let mut c = OpusReframe::new();
        assert_eq!(c.stamp(41_794_000_000, BLOCK, RATE, FRAME_BYTES), 41_794_000_000);
    }

    #[test]
    fn later_blocks_are_twenty_ms_apart_despite_160ms_source_pts() {
        let mut c = OpusReframe::new();
        let got: Vec<i64> = (0..8).map(|i| c.stamp(i * 160_000_000, BLOCK, RATE, FRAME_BYTES)).collect();
        let want: Vec<i64> = (0..8).map(|i| i * 20_000_000).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn a_partial_block_shortens_only_its_own_duration() {
        let mut c = OpusReframe::new();
        assert_eq!(c.stamp(0, BLOCK, RATE, FRAME_BYTES), 0);
        assert_eq!(c.stamp(160_000_000, BLOCK / 2, RATE, FRAME_BYTES), 20_000_000);
        assert_eq!(c.stamp(320_000_000, BLOCK, RATE, FRAME_BYTES), 30_000_000);
    }

    #[test]
    fn reset_reanchors_at_the_next_block() {
        let mut c = OpusReframe::new();
        c.stamp(0, BLOCK, RATE, FRAME_BYTES);
        c.reset();
        assert_eq!(c.stamp(41_708_000_000, BLOCK, RATE, FRAME_BYTES), 41_708_000_000);
    }
}
