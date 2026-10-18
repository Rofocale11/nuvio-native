use crate::addon_http as http;
use crate::model::{Error, Result};
use serde_json::{json, Value};

pub struct LinkCodeResponse {
    pub code: String,
    pub link: String,
    pub qrcode: String,
}

pub fn create_link_code() -> Result<LinkCodeResponse> {
    let url = "https://link.stremio.com/api/v2/create?type=Create";

    let headers = vec![
        "Content-Type: application/json".to_string(),
        "Accept: application/json".to_string(),
    ];

    let res = http::request_status(url, "GET", &headers, None, http::CONTROL)
        .map_err(|_| Error::Network)?;

    let raw_response = String::from_utf8_lossy(&res.1);
    plx_base::eventlog::log(&format!("stremio: RAW RESPONSE -> {}", raw_response));

    if res.0 != 200 {
        return Err(Error::Status(res.0));
    }

    let v: Value = serde_json::from_slice(&res.1).map_err(|e| Error::Parse(e.to_string()))?;
    let result = if v.get("result").is_some() { &v["result"] } else { &v };

    Ok(LinkCodeResponse {
        code: result["code"].as_str().unwrap_or("MISSING").to_string(),
        link: result["link"].as_str().unwrap_or("MISSING").to_string(),
        qrcode: result["qrcode"].as_str().unwrap_or("MISSING").to_string(),
    })
}

pub fn poll_link_code(code: &str) -> Result<Option<String>> {
    let url = format!("https://link.stremio.com/api/v2/read?type=Read&code={}", code);

    let headers = vec![
        "Content-Type: application/json".to_string(),
        "Accept: application/json".to_string(),
    ];

    let res = http::request_status(&url, "GET", &headers, None, http::CONTROL)
        .map_err(|_| Error::Network)?;

    if res.0 == 404 {
        return Ok(None);
    }
    
    if res.0 != 200 {
        return Err(Error::Status(res.0));
    }

    let v: Value = serde_json::from_slice(&res.1).map_err(|e| Error::Parse(e.to_string()))?;
    
    if let Some(auth_key) = v["result"]["authKey"].as_str() {
        Ok(Some(auth_key.to_string()))
    } else {
        Ok(None)
    }
}

// FIX: Changed plx_stremio::AddonSet to crate::AddonSet
pub fn get_addons(auth_key: &str) -> Result<crate::AddonSet> {
    let url = "https://api.strem.io/api/addonCollectionGet";
    let body = json!({
        "authKey": auth_key,
        "update": true
    });

    let headers = vec!["Content-Type: application/json".to_string()];
    let body_bytes = serde_json::to_vec(&body).map_err(|e| Error::Parse(e.to_string()))?;

    let res = http::request_status(url, "POST", &headers, Some(&body_bytes), http::CONTROL)
        .map_err(|_| Error::Network)?;

    if res.0 != 200 {
        return Err(Error::Status(res.0));
    }

    let v: Value = serde_json::from_slice(&res.1).map_err(|e| Error::Parse(e.to_string()))?;
    
    let mut addons = Vec::new();
    if let Some(addons_array) = v["result"]["addons"].as_array() {
        for item in addons_array {
            let mut mapped = item.clone();
            // Stremio calls it "transportUrl", the app expects "url"
            if let Some(url_val) = mapped.get("transportUrl").cloned() {
                mapped["url"] = url_val;
            }
            
            // Parse directly into the Addon struct
            if let Ok(addon) = serde_json::from_value::<crate::model::Addon>(mapped) {
                addons.push(addon);
            } else {
                plx_base::eventlog::log(&format!("stremio: Failed to parse individual addon -> {:?}", item));
            }
        }
    }
    
    // Create the AddonSet directly! No string parsing needed.
    Ok(crate::AddonSet { addons })
}
pub fn get_library(auth_key: &str) -> Result<Vec<Value>> {
    let url = "https://api.strem.io/api/datastoreGet";
    let body = json!({
        "authKey": auth_key,
        "collection": "libraryItem",
        "all": true
    });

    let headers = vec!["Content-Type: application/json".to_string()];
    let body_bytes = serde_json::to_vec(&body).map_err(|e| Error::Parse(e.to_string()))?;

    let res = http::request_status(url, "POST", &headers, Some(&body_bytes), http::CONTROL)
        .map_err(|_| Error::Network)?;

    if res.0 != 200 {
        return Err(Error::Status(res.0));
    }

    let v: Value = serde_json::from_slice(&res.1).map_err(|e| Error::Parse(e.to_string()))?;
    if let Some(arr) = v["result"].as_array() {
        Ok(arr.clone())
    } else {
        Ok(Vec::new())
    }
}

pub fn set_library_item(auth_key: &str, id: &str, name: &str, type_: &str, poster: &str, saved: bool) -> Result<()> {
    let url = "https://api.strem.io/api/datastorePut";
    let body = json!({
        "authKey": auth_key,
        "collection": "libraryItem",
        "changes": [{
            "_id": id,
            "name": name,
            "type": type_,
            "poster": poster,
            "removed": !saved // Stremio uses `removed: true` to delete
        }]
    });

    let headers = vec!["Content-Type: application/json".to_string()];
    let body_bytes = serde_json::to_vec(&body).map_err(|e| Error::Parse(e.to_string()))?;

    let res = http::request_status(url, "POST", &headers, Some(&body_bytes), http::CONTROL)
        .map_err(|_| Error::Network)?;

    if res.0 != 200 {
        return Err(Error::Status(res.0));
    }

    Ok(())
}