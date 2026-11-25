//! TV-side profile PINs, in the one store.
//!
//! Nuvio's public API exposes `pin_enabled` and `pin_locked_until` as READ-ONLY state: it has no
//! call to set, check or unlock a PIN. A profile is therefore locked on this TV only when the
//! viewer has created a TV PIN here. The account flag is advisory and never gates entry — a
//! single-profile account marked "locked" must still be able to open its one profile.
//!
//! What is stored is PBKDF2-HMAC-SHA-256 under a random 16-byte salt, never the PIN itself. The
//! encoding is a small versioned text record so this crate needs no JSON dependency; a malformed
//! record verifies NOTHING rather than everything, which is the direction a lock must fail.

use crate::{Store, Table};
use plx_base::sha256::{ct_eq, pbkdf2_hmac_sha256, sha256};

/// A four-digit PIN, the shape the picker's pad and Settings both produce.
pub fn valid_pin(pin: &str) -> bool {
    pin.len() == 4 && pin.bytes().all(|b| b.is_ascii_digit())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinVerifier {
    salt: [u8; 16],
    hash: [u8; 32],
    iters: u32,
}

impl PinVerifier {
    /// The highest a Cortex-A9 verifies in well under a second on the switch worker.
    pub const ITERS: u32 = 20_000;
    /// A record is this app's own writing; a larger count is a hand edit or a newer build and must
    /// fail rather than park a worker in PBKDF2 for a u32's worth of rounds.
    pub const MAX_ITERS: u32 = 4 * Self::ITERS;

    pub fn new(pin: &str) -> Option<PinVerifier> {
        if !valid_pin(pin) {
            return None;
        }
        let salt = random_salt();
        let hash = pbkdf2_hmac_sha256(pin.as_bytes(), &salt, Self::ITERS);
        Some(PinVerifier { salt, hash, iters: Self::ITERS })
    }

    pub fn verify(&self, pin: &str) -> bool {
        if !valid_pin(pin) || self.iters == 0 || self.iters > Self::MAX_ITERS {
            return false;
        }
        let got = pbkdf2_hmac_sha256(pin.as_bytes(), &self.salt, self.iters);
        ct_eq(&got, &self.hash)
    }

    fn to_bytes(&self) -> Vec<u8> {
        format!("v1:{}:{}:{}", hex(&self.salt), hex(&self.hash), self.iters).into_bytes()
    }

    fn from_bytes(bytes: &[u8]) -> Option<PinVerifier> {
        let text = std::str::from_utf8(bytes).ok()?;
        let mut parts = text.split(':');
        if parts.next()? != "v1" {
            return None;
        }
        let salt = unhex(parts.next()?, 16)?;
        let hash = unhex(parts.next()?, 32)?;
        let iters = parts.next()?.parse::<u32>().ok()?;
        if parts.next().is_some() || iters == 0 || iters > Self::MAX_ITERS {
            return None;
        }
        Some(PinVerifier { salt: salt.try_into().ok()?, hash: hash.try_into().ok()?, iters })
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str, len: usize) -> Option<Vec<u8>> {
    if text.len() != len * 2 || !text.is_ascii() {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok()).collect()
}

fn random_salt() -> [u8; 16] {
    use std::io::Read;
    let mut salt = [0u8; 16];
    let filled = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut salt)).is_ok();
    if !filled {
        // Only a uniqueness fallback: time and pid are not secret, but a salt does not have to be.
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let digest = sha256(format!("{seed}-{}", std::process::id()).as_bytes());
        salt.copy_from_slice(&digest[..16]);
    }
    salt
}

fn key(index: u32) -> String {
    format!("profile-{index}")
}

pub fn get(store: &Store, index: u32) -> Option<PinVerifier> {
    store.get(Table::ProfilePins, &key(index)).and_then(|b| PinVerifier::from_bytes(&b))
}

pub fn put(store: &Store, index: u32, verifier: &PinVerifier) -> Result<(), String> {
    store.put(Table::ProfilePins, &key(index), &verifier.to_bytes())
}

pub fn remove(store: &Store, index: u32) -> Result<(), String> {
    store.remove(Table::ProfilePins, &key(index))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_four_digits_are_a_pin() {
        assert!(valid_pin("0123"));
        for bad in ["", "123", "12345", "12a4", "12 4", "１２３４"] {
            assert!(!valid_pin(bad), "{bad}");
        }
        assert!(PinVerifier::new("1234").is_some());
        assert!(PinVerifier::new("12a4").is_none());
    }

    #[test]
    fn a_verifier_accepts_its_pin_and_nothing_else() {
        let v = PinVerifier::new("4821").unwrap();
        assert!(v.verify("4821"));
        assert!(!v.verify("4822"));
        assert!(!v.verify(""));
        assert_ne!(PinVerifier::new("4821").unwrap().salt, v.salt, "a fresh salt is drawn for each PIN");
    }

    #[test]
    fn a_round_trip_through_the_store_keeps_the_verifier() {
        let dir = plx_base::testscratch::process_dir("nuvio-pin-store");
        let store = Store::open_at(&dir.join("pins.redb")).unwrap();
        let v = PinVerifier::new("1234").unwrap();
        put(&store, 2, &v).unwrap();
        assert_eq!(get(&store, 2), Some(v.clone()));
        assert!(get(&store, 2).unwrap().verify("1234"));
        assert!(get(&store, 1).is_none());
        remove(&store, 2).unwrap();
        assert!(get(&store, 2).is_none());
    }

    #[test]
    fn a_corrupt_record_verifies_nothing() {
        let text = b"v1:zz:00:20000";
        assert!(PinVerifier::from_bytes(text).is_none());
        assert!(PinVerifier::from_bytes(b"garbage").is_none());
        let dir = plx_base::testscratch::process_dir("nuvio-pin-corrupt");
        let store = Store::open_at(&dir.join("pins.redb")).unwrap();
        store.put(Table::ProfilePins, "profile-3", b"v1:zz:00:20000").unwrap();
        assert!(get(&store, 3).is_none());
    }
}
