//! Tolerant decoding of what addons actually send: numbers where the protocol says strings
//! (`imdbRating: 8.1`, `releaseInfo: 2019`), strings where it says numbers (`season: "1"`),
//! `null` anywhere, and a lone string where a list is expected. One bad field must never cost a
//! whole catalog page or stream list.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

fn value_to_string(v: Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn value_to_u64(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64)),
        Value::String(s) => s.trim().parse::<f64>().ok().filter(|f| *f >= 0.0).map(|f| f as u64),
        _ => None,
    }
}

pub fn opt_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(value_to_string(Value::deserialize(d)?))
}

pub fn opt_u32<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    Ok(value_to_u64(&Value::deserialize(d)?).and_then(|n| u32::try_from(n).ok()))
}

pub fn opt_u64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Ok(value_to_u64(&Value::deserialize(d)?))
}

pub fn string_vec<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Array(items) => items
            .into_iter()
            .filter_map(|v| match v {
                // `{ "name": "Drama" }` shapes from some catalogs.
                Value::Object(mut o) => o.remove("name").and_then(value_to_string),
                other => value_to_string(other),
            })
            .filter(|s| !s.trim().is_empty())
            .collect(),
        Value::String(s) if !s.trim().is_empty() => {
            s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
        }
        _ => Vec::new(),
    })
}

pub fn opt_bool<'de, D: Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    Ok(value_to_bool(&Value::deserialize(d)?))
}

fn value_to_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => Some(n.as_f64().unwrap_or(0.0) != 0.0),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// Catalog `extra`: entries may be plain names or objects; `isRequired` may be a string; names are
/// lowercased and de-duplicated the way Nuvio's `mapCatalogExtra` does.
pub fn extras<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<crate::model::ExtraDecl>, D::Error> {
    let mut out: Vec<crate::model::ExtraDecl> = Vec::new();
    if let Value::Array(items) = Value::deserialize(d)? {
        for item in items {
            let (name, required, options) = match &item {
                Value::String(s) => (s.clone(), false, Vec::new()),
                Value::Object(o) => (
                    o.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                    o.get("isRequired").and_then(value_to_bool).unwrap_or(false),
                    o.get("options")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(|v| value_to_string(v.clone())).collect())
                        .unwrap_or_default(),
                ),
                _ => continue,
            };
            let name = name.trim().to_ascii_lowercase();
            if name.is_empty() {
                continue;
            }
            if let Some(e) = out.iter_mut().find(|e| e.name == name) {
                e.is_required |= required;
                if e.options.is_empty() {
                    e.options = options;
                }
            } else {
                out.push(crate::model::ExtraDecl { name, is_required: required, options });
            }
        }
    }
    Ok(out)
}

/// Remove every `null` member of every object, recursively, so `#[serde(default)]` applies to it.
pub fn strip_nulls(v: &mut Value) {
    match v {
        Value::Object(map) => {
            map.retain(|_, x| !x.is_null());
            for x in map.values_mut() {
                strip_nulls(x);
            }
        }
        Value::Array(items) => {
            for x in items.iter_mut() {
                strip_nulls(x);
            }
        }
        _ => {}
    }
}

/// Decode one value after [`strip_nulls`]; `None` if it still does not fit.
pub fn decode<T: DeserializeOwned>(mut v: Value) -> Option<T> {
    strip_nulls(&mut v);
    serde_json::from_value(v).ok()
}

/// Decode every element of an array that fits, dropping the ones that do not.
pub fn decode_list<T: DeserializeOwned>(v: Option<Value>) -> Vec<T> {
    match v {
        Some(Value::Array(items)) => items.into_iter().filter_map(decode).collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{Meta, MetaPreview};

    #[test]
    fn tolerates_real_world_shapes() {
        let v = serde_json::json!({
            "id": "tt1", "type": "movie", "name": "X", "imdbRating": 8.1, "releaseInfo": 2019,
            "genres": null, "poster": null, "description": "d"
        });
        let m: MetaPreview = super::decode(v).unwrap();
        assert_eq!(m.imdb_rating.as_deref(), Some("8.1"));
        assert_eq!(m.release_info.as_deref(), Some("2019"));
        assert!(m.genres.is_empty() && m.poster.is_none());

        let v = serde_json::json!({
            "id": "tt2", "type": "series", "name": "S", "cast": "A, B",
            "videos": [{"id": "tt2:1:1", "season": "1", "episode": 1.0, "title": "Pilot"}]
        });
        let m: Meta = super::decode(v).unwrap();
        assert_eq!(m.cast, vec!["A", "B"]);
        assert_eq!(m.videos[0].season, Some(1));
        assert_eq!(m.videos[0].episode, Some(1));
    }
}
