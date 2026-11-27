//! Fork Rofocale11: off-frame teardown for hero trailer previews.
//!
//! A trailer autoplay is stopped by an ordinary key press (focus leaving the hero). Upstream's
//! `NativeStop::unblock_join_close` joins the demux reader on that frame. The reader is parked in a
//! googlevideo HTTPS range read, so the join held the UI frame for ~44 ms on the dev set (the
//! `join_demux:43.9ms` inside `dframe:56.8ms` of the 2026-11-27 detail-nav-deep run). A real stop's
//! Unload already runs off-frame; this module gives the demux join the same treatment for previews.
//!
//! The join cannot simply be deferred: `demux` holds raw `SendPtr`s into the two AU queues and the
//! `HttpStream` (`ff::demux`), and `Engine::drop` panics in test builds while `stream_th` is still
//! owned for exactly that reason. `ParkedDemux` therefore owns the whole set the reader touches —
//! thread, both queues and the socket — and only releases them after the thread has finished and
//! been joined. `NativeStop` moves the set here in `unblock_join_close`; `FeedResources` holds it
//! until the per-frame reaper (reached from `poll_player_release`) joins and drains it.

use std::thread::JoinHandle;

use crate::aq::AuQueue;
use crate::feed_resources::FeedResources;
use plx_net::stream::HttpStream;

/// A preview's demux reader, parked after `aq_abort` + transport shutdown have woken it, together
/// with every allocation it may still write through its raw pointers.
pub(crate) struct ParkedDemux {
    thread: Option<JoinHandle<()>>,
    aq_video: Option<Box<AuQueue>>,
    aq_audio: Option<Box<AuQueue>>,
    /// The `HttpStream` the reader's AVIO uses. Moving the `Box` does not move the pointee, so
    /// `hs_ptr` stays valid until `finish` closes it. Held for RAII, never read directly.
    #[allow(dead_code)]
    hs: Box<HttpStream>,
    hs_ptr: *mut HttpStream,
    parked_at: std::time::Instant,
}

impl ParkedDemux {
    pub(crate) fn new(
        thread: JoinHandle<()>,
        aq_video: Option<Box<AuQueue>>,
        aq_audio: Option<Box<AuQueue>>,
        hs: Box<HttpStream>,
        hs_ptr: *mut HttpStream,
    ) -> Self {
        // Fork Rofocale11: pin the wake-up that makes parking safe. `unblock_join_close` calls
        // `aq_abort` on both lanes before it parks, so a reader held in `aq_push_with_drain` has a
        // condvar broadcast and will unwind without the frame waiting for it.
        debug_assert!(
            aq_video
                .as_ref()
                .is_none_or(|q| unsafe { crate::aq::aq_is_aborted(&**q as *const AuQueue) })
                && aq_audio
                    .as_ref()
                    .is_none_or(|q| unsafe { crate::aq::aq_is_aborted(&**q as *const AuQueue) }),
            "a demux may only be parked after both queues have been aborted",
        );
        Self {
            thread: Some(thread),
            aq_video,
            aq_audio,
            hs,
            hs_ptr,
            parked_at: std::time::Instant::now(),
        }
    }

    pub(crate) fn thread_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Join (a no-op once finished), then free what the reader held. One definition shared by the
    /// reaper and the safety-net `Drop`, so neither can double-close or double-destroy.
    fn finish(&mut self) {
        if let Some(thread) = self.thread.take() {
            plx_base::task::join("demux", thread);
        }
        let p = std::mem::take(&mut self.hs_ptr);
        if !p.is_null() {
            plx_net::stream::http_close(p);
        }
        crate::native_executor::drain_one(self.aq_video.as_mut());
        crate::native_executor::drain_one(self.aq_audio.as_mut());
        for q in [self.aq_video.as_mut(), self.aq_audio.as_mut()]
            .into_iter()
            .flatten()
        {
            crate::aq::aq_destroy(&mut **q);
        }
        self.aq_video = None;
        self.aq_audio = None;
    }
}

impl Drop for ParkedDemux {
    fn drop(&mut self) {
        // Only a missed reap reaches here with a live handle. `finish` joins (safe but blocking)
        // rather than freeing memory the reader may still write; the warning names that path.
        if self.thread.is_some() {
            plx_base::eventlog::log(
                "preview teardown: a parked demux was dropped before its reaper; joining it now",
            );
        }
        self.finish();
    }
}

/// Reap a parked demux whose reader has finished. Called from the existing per-frame
/// `reap_abandoned_load` hook (run.rs's `poll_player_release`) and from `begin_native_open`;
/// joining an already-finished thread is free. Returns whether a park was completed.
pub(crate) fn reap(feed: &mut FeedResources) -> bool {
    let finished = feed
        .parked_demux_mut()
        .is_some_and(|parked| parked.thread_finished());
    if !finished {
        return false;
    }
    let mut parked = feed.take_parked_demux().expect("checked above");
    let waited = parked.parked_at.elapsed();
    parked.finish();
    plx_base::eventlog::log(&format!(
        "preview teardown: demux joined off the stop frame after {}ms",
        waited.as_millis(),
    ));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finishing_a_park_is_idempotent_and_leaves_no_live_owner() {
        // The reaper, the Drop safety net and a second reap can all reach `finish`; each must be a
        // no-op after the first, or the socket would be double-closed and the queues double-freed.
        let mut parked = ParkedDemux::new(
            std::thread::spawn(|| {}),
            None,
            None,
            plx_net::stream::http_stream_boxed(),
            std::ptr::null_mut(),
        );
        parked.finish();
        assert!(parked.thread.is_none());
        assert!(parked.hs_ptr.is_null());
        parked.finish();
        drop(parked);
    }

    #[test]
    #[should_panic(expected = "a demux may only be parked after both queues have been aborted")]
    fn a_reader_cannot_be_parked_before_its_queues_are_aborted() {
        let q = crate::aq::aq_new(64);
        let _ = ParkedDemux::new(
            std::thread::spawn(|| {}),
            Some(q),
            None,
            plx_net::stream::http_stream_boxed(),
            std::ptr::null_mut(),
        );
    }
}
