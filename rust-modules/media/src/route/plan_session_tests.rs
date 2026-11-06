//! Prime-decision and playback-session classification tests: clip queue rows, control-
//! snapshot expiry, and prime refusal causes.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn a_clip_queue_row_does_not_arm_up_next() {
    let clip = plx_plex::plex::QueueRow {
        kind: "clip".into(),
        rk: "9".into(),
        part: "/p".into(),
        ..Default::default()
    };
    assert!(up_next_of(&clip).is_none());
    let movie = plx_plex::plex::QueueRow {
        kind: "movie".into(),
        rk: "1".into(),
        ..Default::default()
    };
    assert!(up_next_of(&movie).is_none());
}


#[test]
fn a_route_change_wins_over_an_expired_control_snapshot() {
    assert!(matches!(
        classify_prime_decision(false, plx_plex::plex::JsonDeadlineOutcome::Deadline),
        Err(PrimeRefusal::Session),
    ));
}


#[test]
fn prime_refusals_follow_the_issued_cause_not_the_clock_at_return() {
    let response = |status, body: &[u8]| plx_plex::plex::JsonDeadlineOutcome::Response {
        reply: plx_plex::http::Reply {
            status,
            body: body.to_vec(),
            peer_pin: None,
        },
        parsed: None,
    };
    assert!(matches!(
        classify_prime_decision(true, response(500, b"nope")),
        Err(PrimeRefusal::Control),
    ));
    assert!(matches!(
        classify_prime_decision(true, response(200, b"not-json")),
        Err(PrimeRefusal::Control),
    ));
    assert!(matches!(
        classify_prime_decision(true, plx_plex::plex::JsonDeadlineOutcome::Transport),
        Err(PrimeRefusal::Control),
    ));
    assert!(matches!(
        classify_prime_decision(true, plx_plex::plex::JsonDeadlineOutcome::Deadline),
        Err(PrimeRefusal::Deadline),
    ));
    assert!(matches!(
        classify_prime_decision(false, response(500, b"nope")),
        Err(PrimeRefusal::Session),
    ));
    assert!(matches!(
        classify_prime_decision(false, plx_plex::plex::JsonDeadlineOutcome::Transport),
        Err(PrimeRefusal::Session),
    ));
}


/// **The local Nuvio account's preferences are `nuvio-session.json`.** The shim's `/api/v2/user`
/// carries no `profile` block, and a Nuvio profile still has a synthetic shim credential — so the
/// account has to be recognised as local BEFORE any HTTP preference read, or the subtitle
/// language saved in Settings reads as unset (the player's subtitle search then opens on the
/// audio language instead of the preference).
#[test]
fn the_local_nuvio_account_is_recognised_before_any_credential_read() {
    let user = |uuid: &str| plx_plex::plex::session::UserRef { uuid: uuid.into(), ..Default::default() };
    assert!(is_local_nuvio(Some(&user("nuvio"))));
    assert!(is_local_nuvio(Some(&user("nuvio-profile-2"))));
    assert!(!is_local_nuvio(Some(&user("plex-owner"))));
    assert!(!is_local_nuvio(Some(&user(""))));
    assert!(!is_local_nuvio(None));
}
