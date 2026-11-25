//! the canonical storage backend, run IN PROCESS against the single redb file.
//!
//! Upstream's `plxstorage` helper is a separate process that translates a closed wire protocol
//! into DB8 and LG Keymanager3 calls. Nuvio Native has no DB8 record and no helper in its package,
//! so this module implements the same `Rpc` seam over `plx_nuvio_store`: DB8's two kinds become two
//! redb tables, and Keymanager3's AES-256-GCM envelopes are produced by `ring` with keys kept in a
//! third table. Everything above `Rpc` — the CAS ledger, digests, reconciliation, readback checks
//! and the 256 KiB document guard — is upstream's unchanged `storage_service::backend::Backend`,
//! so the session and consent owners see exactly the protocol they always saw.

use super::{
    backend::Backend,
    keymanager::Rpc,
    state::{CanonicalState, Flavor},
    wire::{self, ErrorCode, Request, Response},
};
use plx_base::eventlog::log;
use plx_base::paths;
use plx_nuvio_store::{Table, Store};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// One process-wide backend. The wire protocol is serial, and the mutex keeps a DB8-style CAS
/// read/write pair atomic even when two workers commit at once.
static BACKEND: OnceLock<Mutex<Backend<RedbRpc>>> = OnceLock::new();

fn store() -> Result<Arc<Store>, String> {
    if let Some(store) = plx_nuvio_store::get_arc() {
        return Ok(store);
    }
    let store = plx_nuvio_store::Store::open()?;
    let _ = plx_nuvio_store::install(store.clone());
    Ok(store)
}

fn backend() -> Result<&'static Mutex<Backend<RedbRpc>>, String> {
    if let Some(backend) = BACKEND.get() {
        return Ok(backend);
    }
    let s = store()?;
    let app = paths::app_id();
    let flavor = Flavor::from_app_id(app).ok_or_else(|| format!("unknown app id {app}"))?;
    let service = format!("{app}.storage");
    let b = Backend::new(
        RedbRpc::new(s, flavor, service.clone()),
        flavor,
        service,
    );
    let _ = BACKEND.set(Mutex::new(b));
    BACKEND.get().ok_or_else(|| "backend not installed".into())
}

/// the entry point used by `storage::client::transact`. The socket path is gone on
/// this build; a failure here is a local store failure, never a helper timeout.
pub fn dispatch(request: Request) -> Result<Response, String> {
    let b = backend()?;
    let mut guard = b.lock().unwrap_or_else(|e| e.into_inner());
    Ok(guard.dispatch(request))
}

struct RedbRpc {
    store: Arc<Store>,
    /// the canonical record is flavour- and service-scoped. A store file copied
    /// between installs (debug -> stable, or an older schema) can carry an object under our id
    /// that this app cannot read. Validate on read and quarantine just that object instead of
    /// rejecting every sign-in for the lifetime of the file.
    flavor: Flavor,
    service: String,
    handles: HashMap<String, OpenHandle>,
}

struct OpenHandle {
    name: String,
    iv: Vec<u8>,
    encrypt: bool,
}

impl RedbRpc {
    fn new(store: Arc<Store>, flavor: Flavor, service: String) -> Self {
        Self { store, flavor, service, handles: HashMap::new() }
    }

    /// An object is usable only if it belongs to THIS service and THIS flavour. The upstream
    /// `Backend` checks the same facts and turns a mismatch into `Corrupt`, which the login
    /// screen reports as "backend rejected"; nothing in it resets the bad record. We check one
    /// layer lower, where the fork owns the file, so a poisoned record can be retired.
    fn usable(&self, id: &str, object: &Value) -> bool {
        if object.get("_kind").and_then(Value::as_str) != Some(format!("{}:1", self.service).as_str()) {
            return false;
        }
        if object.get("_id").and_then(Value::as_str) != Some(id) {
            return false;
        }
        match (object.get("state").and_then(Value::as_str), object.get("nuvio").and_then(Value::as_str)) {
            (Some(state), _) => CanonicalState::decode(state.as_bytes(), self.flavor).is_ok(),
            (_, Some(payload)) => payload.len() <= super::state::MAX_LOGICAL_BYTES,
            _ => false,
        }
    }

    /// Move one unreadable object aside. The rest of the single store — addons, watch state,
    /// install preferences, caches — is untouched, and the quarantined copy stays available for
    /// inspection. The next read sees no object, so `Backend` creates a fresh Cleared state.
    fn quarantine(&self, id: &str, object: &Value) {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        let kept_as = format!("{id}.rejected-{stamp}");
        if let Ok(bytes) = serde_json::to_vec(object) {
            let _ = self.store.put(Table::HelperObjects, &kept_as, &bytes);
        }
        let _ = self.store.remove(Table::HelperObjects, id);
        log(&format!(
            "nuvio: quarantined unreadable storage object {id} as {kept_as}; starting from a fresh record"
        ));
    }

    // ---- the DB8-shaped half -----------------------------------------------------------------

    fn object(&self, id: &str) -> Option<Value> {
        let bytes = self.store.get(Table::HelperObjects, id)?;
        serde_json::from_slice(&bytes).ok()
    }

    fn put_objects(&mut self, objects: &Value) -> Result<Value, ErrorCode> {
        let Some(list) = objects.as_array() else { return Err(ErrorCode::Invalid) };
        for object in list {
            let id = object["_id"].as_str().ok_or(ErrorCode::Invalid)?;
            let current = self.object(id);
            if let (Some(current), Some(expected)) = (&current, object.get("_rev").and_then(Value::as_u64)) {
                let actual = current["_rev"].as_u64().ok_or(ErrorCode::Corrupt)?;
                if actual != expected {
                    // DB8's revision fence: a stale write is refused, and the backend re-reads.
                    return Ok(json!({"returnValue": false, "errorCode": 409}));
                }
            }
            let mut next = object.clone();
            let rev = current.as_ref().and_then(|c| c["_rev"].as_u64()).unwrap_or(0) + 1;
            next.as_object_mut().ok_or(ErrorCode::Invalid)?.insert("_rev".into(), json!(rev));
            let bytes = serde_json::to_vec(&next).map_err(|_| ErrorCode::Invalid)?;
            self.store
                .put(Table::HelperObjects, id, &bytes)
                .map_err(|_| ErrorCode::Unavailable)?;
        }
        Ok(json!({"returnValue": true}))
    }

    // ---- the Keymanager3-shaped half ---------------------------------------------------------

    fn key(&self, name: &str) -> Result<LessSafeKey, ErrorCode> {
        let bytes = self.store.get(Table::HelperKeys, name).ok_or(ErrorCode::Capability)?;
        let key = UnboundKey::new(&AES_256_GCM, &bytes).map_err(|_| ErrorCode::Invalid)?;
        Ok(LessSafeKey::new(key))
    }

    fn finish(&mut self, payload: &Value) -> Result<Value, ErrorCode> {
        let handle = payload["handle"].as_str().ok_or(ErrorCode::Invalid)?;
        let open = self.handles.remove(handle).ok_or(ErrorCode::Invalid)?;
        let data = plx_base::b64::decode(payload["data"].as_str().ok_or(ErrorCode::Invalid)?)
            .ok_or(ErrorCode::Invalid)?;
        let key = self.key(&open.name)?;
        let nonce = Nonce::try_assume_unique_for_key(&open.iv).map_err(|_| ErrorCode::Invalid)?;
        let output = if open.encrypt {
            let mut buf = data;
            let _tag = key.seal_in_place_append_tag(nonce, Aad::empty(), &mut buf)
                .map_err(|_| ErrorCode::Invalid)?;
            buf
        } else {
            let mut buf = data;
            let plain = key.open_in_place(nonce, Aad::empty(), &mut buf)
                .map_err(|_| ErrorCode::Invalid)?;
            plain.to_vec()
        };
        Ok(json!({"returnValue": true, "output": plx_base::b64::encode(&output)}))
    }
}

impl Rpc for RedbRpc {
    fn call(&mut self, uri: &str, payload: &Value) -> Result<Value, ErrorCode> {
        match uri {
            "luna://com.palm.db/putKind" => Ok(json!({"returnValue": true})),
            "luna://com.palm.db/get" => {
                let Some(ids) = payload["ids"].as_array() else { return Err(ErrorCode::Invalid) };
                let mut results = Vec::new();
                for id in ids.iter().filter_map(Value::as_str) {
                    if let Some(object) = self.object(id) {
                        // self-heal one bad record instead of bricking sign-in.
                        if !self.usable(id, &object) {
                            self.quarantine(id, &object);
                            continue;
                        }
                        results.push(object);
                    }
                }
                Ok(json!({"returnValue": true, "results": results}))
            }
            "luna://com.palm.db/put" => self.put_objects(&payload["objects"]),
            u if u.ends_with("/generateKey") => {
                let name = payload["name"].as_str().ok_or(ErrorCode::Invalid)?;
                if self.store.get(Table::HelperKeys, name).is_some() {
                    return Ok(json!({"returnValue": false, "errorCode": -10000}));
                }
                let mut bytes = [0u8; 32];
                SystemRandom::new().fill(&mut bytes).map_err(|_| ErrorCode::Unavailable)?;
                self.store
                    .put(Table::HelperKeys, name, &bytes)
                    .map_err(|_| ErrorCode::Unavailable)?;
                Ok(json!({"returnValue": true}))
            }
            u if u.ends_with("/begin") => {
                let name = payload["name"].as_str().ok_or(ErrorCode::Invalid)?;
                if self.store.get(Table::HelperKeys, name).is_none() {
                    return Ok(json!({"returnValue": false, "errorCode": -10001}));
                }
                let encrypt = payload["params"]["purpose"]
                    .as_array()
                    .map(|p| p.iter().any(|v| v.as_str() == Some("encrypt")));
                let iv = if encrypt == Some(true) {
                    let mut iv = [0u8; 12];
                    SystemRandom::new().fill(&mut iv).map_err(|_| ErrorCode::Unavailable)?;
                    iv.to_vec()
                } else {
                    let text = payload["params"]["iv"].as_str().ok_or(ErrorCode::Invalid)?;
                    plx_base::b64::decode(text).ok_or(ErrorCode::Invalid)?
                };
                if iv.is_empty() {
                    return Err(ErrorCode::Invalid);
                }
                let handle = format!("{}", self.handles.len() + 1);
                self.handles.insert(handle.clone(), OpenHandle { name: name.into(), iv: iv.clone(), encrypt: encrypt == Some(true) });
                Ok(json!({"returnValue": true, "handle": handle, "iv": plx_base::b64::encode(&iv)}))
            }
            u if u.ends_with("/finish") => self.finish(payload),
            u if u.ends_with("/abort") || u.ends_with("/removeKey") => {
                if u.ends_with("/removeKey") {
                    if let Some(name) = payload["name"].as_str() {
                        let _ = self.store.remove(Table::HelperKeys, name);
                    }
                } else if let Some(handle) = payload["handle"].as_str() {
                    self.handles.remove(handle);
                }
                Ok(json!({"returnValue": true}))
            }
            other => {
                let _ = (other, &wire::PROTOCOL);
                Err(ErrorCode::Capability)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::state::Generation;

    const SERVICE: &str = "com.nuvionative.app.storage";

    #[test]
    fn a_redb_object_round_trips_with_a_revision_fence() {
        let dir = plx_base::testscratch::process_dir("nuvio-redb-backend");
        let store = Store::open_at(&dir.join("test.redb")).unwrap();
        let mut rpc = RedbRpc::new(store, Flavor::Stable, SERVICE.into());
        // The addon-shaped object exercises the CAS fence without carrying a canonical state,
        // which the read-side self-heal now validates.
        let one = json!({"_id":"nvaddons.stable","_kind":"com.nuvionative.app.storage:1","nuvio":"{}" });
        assert_eq!(rpc.call("luna://com.palm.db/put", &json!({"objects":[one.clone()]})).unwrap()["returnValue"], true);
        let got = rpc.call("luna://com.palm.db/get", &json!({"ids":["nvaddons.stable"]})).unwrap();
        assert_eq!(got["results"][0]["nuvio"], "{}");
        assert_eq!(got["results"][0]["_rev"], 1);
        let mut stale_one = one;
        stale_one["_rev"] = json!(0);
        let stale = rpc.call("luna://com.palm.db/put", &json!({"objects":[stale_one]})).unwrap();
        assert_eq!(stale["returnValue"], false);
    }

    #[test]
    fn a_fresh_store_reports_missing_not_a_helper_failure() {
        let dir = plx_base::testscratch::process_dir("nuvio-redb-fresh");
        let store = Store::open_at(&dir.join("fresh.redb")).unwrap();
        let _ = plx_nuvio_store::install(store);
        let response = super::dispatch(Request::Load {}).unwrap();
        assert!(matches!(response, Response::Missing), "fresh redb: {response:?}");
    }

    #[test]
    fn keymanager_envelopes_round_trip_from_the_single_store() {
        let dir = plx_base::testscratch::process_dir("nuvio-redb-keymanager");
        let store = Store::open_at(&dir.join("keys.redb")).unwrap();
        let mut rpc = RedbRpc::new(store, Flavor::Stable, SERVICE.into());
        let context = json!({"auth_generation":"0123456789abcdef0123456789abcdef","flavor":"stable"});
        let sealed = super::super::keymanager::seal(&mut rpc, "a.0123456789012345678901", "a-shim-token", context.clone()).unwrap();
        let opened = super::super::keymanager::unseal(&mut rpc, "a.0123456789012345678901", &sealed, &context).unwrap();
        assert_eq!(opened, "a-shim-token");
        assert!(sealed.contains("keymanager3-aes256-gcm-v1"));
    }

    /// The failure the owner reproduced: a record left by another install under our object id.
    /// One bad row must not turn every later sign-in into "backend rejected".
    #[test]
    fn a_foreign_kind_record_is_quarantined_and_the_rest_of_the_store_survives() {
        let dir = plx_base::testscratch::process_dir("nuvio-redb-heal-kind");
        let store = Store::open_at(&dir.join("heal.redb")).unwrap();
        let mut rpc = RedbRpc::new(store.clone(), Flavor::Stable, SERVICE.into());
        let foreign = json!({
            "_id": "plxstate.stable",
            "_kind": "com.nuvionative.app.debug.storage:1",
            "_rev": 1,
            "state": "{}",
        });
        assert_eq!(rpc.call("luna://com.palm.db/put", &json!({"objects":[foreign.clone()]})).unwrap()["returnValue"], true);
        // An unrelated fork row in the SAME file must not be swept away with it.
        store.put(Table::AccountSession, "session", b"keep-me").unwrap();

        let got = rpc.call("luna://com.palm.db/get", &json!({"ids":["plxstate.stable"]})).unwrap();
        assert!(got["results"].as_array().unwrap().is_empty(), "bad record was still served: {got}");

        let held: Vec<(String, Vec<u8>)> = store.list(Table::HelperObjects)
            .into_iter().filter(|(key, _)| key.starts_with("plxstate.stable.rejected-")).collect();
        assert_eq!(held.len(), 1, "the bad record must be kept for inspection, not destroyed");
        assert_eq!(serde_json::from_slice::<Value>(&held[0].1).unwrap(), foreign);
        assert_eq!(store.get(Table::AccountSession, "session").unwrap(), b"keep-me");
    }

    /// A stable id carrying a debug-flavoured state (the cross-flavour copy) is unreadable by
    /// `CanonicalState::decode`; `Backend` would call it Corrupt. The storage layer retires it
    /// first, so the session owner sees a fresh store and can sign in normally.
    #[test]
    fn a_wrong_flavour_state_is_quarantined_and_loads_as_missing() {
        let dir = plx_base::testscratch::process_dir("nuvio-redb-heal-flavour");
        let store = Store::open_at(&dir.join("heal.redb")).unwrap();
        let mut rpc = RedbRpc::new(store.clone(), Flavor::Stable, SERVICE.into());
        let debug = CanonicalState::new(Flavor::Debug, Generation([1; 16]));
        let object = json!({
            "_id": "plxstate.stable",
            "_kind": format!("{SERVICE}:1"),
            "state": serde_json::to_string(&debug).unwrap(),
        });
        assert_eq!(rpc.call("luna://com.palm.db/put", &json!({"objects":[object]})).unwrap()["returnValue"], true);

        let mut backend = Backend::new(rpc, Flavor::Stable, SERVICE.into());
        assert!(matches!(backend.load(), Ok(None)), "the unreadable record must read as a fresh store");
        assert_eq!(store.list(Table::HelperObjects).iter()
            .filter(|(key, _)| key.starts_with("plxstate.stable.rejected-")).count(), 1);
    }

    /// The inverse: a healthy record is untouched, so the self-heal never silently resets a
    /// store this app can actually read.
    #[test]
    fn a_valid_canonical_state_is_not_quarantined() {
        let dir = plx_base::testscratch::process_dir("nuvio-redb-heal-valid");
        let store = Store::open_at(&dir.join("valid.redb")).unwrap();
        let mut rpc = RedbRpc::new(store.clone(), Flavor::Stable, SERVICE.into());
        let state = CanonicalState::new(Flavor::Stable, Generation([2; 16]));
        let object = json!({
            "_id": "plxstate.stable",
            "_kind": format!("{SERVICE}:1"),
            "state": serde_json::to_string(&state).unwrap(),
        });
        assert_eq!(rpc.call("luna://com.palm.db/put", &json!({"objects":[object]})).unwrap()["returnValue"], true);

        let mut backend = Backend::new(rpc, Flavor::Stable, SERVICE.into());
        assert!(backend.load().unwrap().is_some(), "a readable record must survive the self-heal");
        assert!(store.list(Table::HelperObjects).iter()
            .all(|(key, _)| !key.contains(".rejected-")));
    }
}
