//! the Nuvio Native brand transform for the localization catalog.
//!
//! The upstream catalog compiler reads `locales/<tag>/*.json` verbatim. Editing those files would
//! put ~120 brand strings in the merge conflict surface and would freeze upstream wording: a fix
//! upstream makes to a sentence would be hidden by our copy. This module is fork-owned, runs AFTER
//! upstream's `discover()` and BEFORE its `compile()`, and changes only what the fork must change:
//!
//!  * ordered substring replacements, so upstream sentences keep flowing through;
//!  * whole-value overrides for the few messages that would otherwise make a false claim (an
//!    upstream Plex server, an upstream developer, an upstream source URL);
//!  * added keys for notices the fork owes its users.
//!
//! Because this runs before `compile()`, upstream's checks still apply: every key must exist in
//! every shipped language, placeholders must match, and plural forms must be complete. The final
//! scan fails the build if any user-facing string still mentions Plex, which is the promise the UI
//! makes: this app has no Plex account, no Plex server and no plex.tv service.

use crate::catalog::Catalog;
use serde_json::Value;
use std::path::Path;

const CONFIG: &str = "../../nuvio-locales/brand.json";
const SOURCE: &str = "brand/brand.json";

/// Apply the fork's brand transform to every loaded catalog.
pub fn apply(catalogs: &mut [Catalog]) -> Result<(), Vec<String>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(CONFIG);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| vec![format!("{}: {e}; the fork brand config is required", path.display())])?;
    let cfg: Value =
        serde_json::from_str(&text).map_err(|e| vec![format!("{}: {e}", path.display())])?;

    let mut pairs: Vec<(String, String)> = Vec::new();
    if let Some(items) = cfg.get("replace").and_then(Value::as_array) {
        for (i, item) in items.iter().enumerate() {
            let pair = item.as_array().and_then(|a| {
                match (a.first().and_then(Value::as_str), a.get(1).and_then(Value::as_str)) {
                    (Some(from), Some(to)) => Some((from.to_string(), to.to_string())),
                    _ => None,
                }
            });
            match pair {
                Some(p) => pairs.push(p),
                None => return Err(vec![format!("{}: replace[{i}] must be [from, to]", path.display())]),
            }
        }
    }
    let overrides = cfg.get("override").and_then(Value::as_object);
    let added = cfg.get("add").and_then(Value::as_object);
    if overrides.is_none() || added.is_none() {
        return Err(vec![format!("{}: missing override or add object", path.display())]);
    }

    let mut errors = Vec::new();
    for catalog in catalogs.iter_mut() {
        let tag = catalog.spec.tag.clone();
        let english = tag == "en";
        let mut moved_origin = Vec::new();

        for (key, value) in catalog.entries.iter_mut() {
            match overrides.and_then(|o| o.get(key)).and_then(|v| override_for(v, &tag)) {
                Some(replacement) => {
                    // Keep English translator metadata (`description`, `args`, `translate`) intact;
                    // only the message text is ours to change.
                    if english && value.is_object() {
                        if let Value::Object(m) = value {
                            m.insert("value".to_string(), replacement.clone());
                        }
                    } else {
                        *value = replacement.clone();
                    }
                    moved_origin.push(key.clone());
                }
                None => replace_in(value, &pairs, english),
            }
        }
        for key in moved_origin {
            catalog.origin.insert(key, SOURCE.into());
        }

        if let Some(additions) = added {
            for (key, spec) in additions {
                if catalog.entries.contains_key(key) {
                    errors.push(format!("{SOURCE}: {key}: already in locales/{tag}; delete the override"));
                    continue;
                }
                let Some(value) = added_value(spec, &tag, english) else {
                    errors.push(format!("{SOURCE}: {key}: no value for {tag}"));
                    continue;
                };
                catalog.entries.insert(key.clone(), value);
                catalog.origin.insert(key.clone(), SOURCE.into());
            }
        }
    }

    // The promise: no string inherited from upstream names Plex in any shipped language. Strings
    // this module supplied are reviewed fork text — they may legitimately say "fork of PlxNative"
    // or "not affiliated with Plex" as attribution.
    for catalog in catalogs.iter() {
        let english = catalog.spec.tag == "en";
        for (key, value) in &catalog.entries {
            if catalog.origin.get(key).is_some_and(|o| o == SOURCE) {
                continue;
            }
            for text in user_facing(value, english) {
                if mentions_plex(text) {
                    errors.push(format!(
                        "{}: {}/{}: still mentions Plex: {}",
                        SOURCE,
                        catalog.spec.tag,
                        key,
                        text.chars().take(90).collect::<String>()
                    ));
                }
            }
        }
    }

    if errors.is_empty() { Ok(()) } else { Err(errors) }
}

/// Whole-word only: "desmultiplexació" is Catalan for demultiplexing, not a product name.
fn mentions_plex(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    for needle in ["plex", "plxnative"] {
        let mut from = 0;
        while let Some(i) = lower[from..].find(needle) {
            let start = from + i;
            let end = start + needle.len();
            let before = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
            let after = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
            if before && after {
                return true;
            }
            from = end;
        }
    }
    false
}

fn override_for<'a>(spec: &'a Value, tag: &str) -> Option<&'a Value> {
    match spec {
        Value::String(_) => Some(spec),
        // A language missing from a per-language override keeps upstream's translation; an
        // English fallback here would silently ship English copy in eleven UIs.
        Value::Object(m) => m.get(tag),
        _ => None,
    }
}

fn added_value(spec: &Value, tag: &str, english: bool) -> Option<Value> {
    if english {
        let description = spec.get("description")?.as_str()?;
        return Some(Value::Object(serde_json::Map::from_iter([
            ("description".to_string(), Value::String(description.to_string())),
            ("value".to_string(), override_for(spec, tag)?.clone()),
        ])));
    }
    override_for(spec, tag).cloned()
}

fn replace_in(value: &mut Value, pairs: &[(String, String)], english: bool) {
    match value {
        Value::String(s) => {
            for (from, to) in pairs {
                if s.contains(from.as_str()) {
                    *s = s.replace(from.as_str(), to.as_str());
                }
            }
        }
        // An English entry has translator metadata (`description`, `args`, `translate`) that is
        // not shown to users; only its `value` is.
        Value::Object(m) if english => {
            if let Some(v) = m.get_mut("value") {
                replace_in(v, pairs, english);
            }
        }
        Value::Object(m) => {
            for v in m.values_mut() {
                replace_in(v, pairs, english);
            }
        }
        _ => {}
    }
}

fn user_facing(value: &Value, english: bool) -> Vec<&str> {
    match value {
        Value::String(s) => vec![s.as_str()],
        Value::Object(m) if english => m.get("value").map(|v| user_facing(v, false)).unwrap_or_default(),
        Value::Object(m) => m.values().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}
