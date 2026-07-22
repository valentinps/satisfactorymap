//! Client for the official Satisfactory Dedicated Server HTTPS API -- the
//! same endpoint the in-game Server Manager uses, documented by Coffee Stain
//! in `CommunityResources/DedicatedServerAPIDocs.md` of the server install.
//! Powers "load the newest save straight off my server" in the desktop app:
//! `PasswordLogin` -> `EnumerateSessions` -> `DownloadSaveGame`.
//!
//! Two protocol quirks shape this module:
//! - The server's TLS certificate is self-signed AND X.509 v1 (Coffee Stain
//!   generates it on first boot). Chain verification is impossible, and
//!   rustls refuses to parse v1 certs at all, so this uses the platform TLS
//!   (native-tls / schannel) with verification disabled. To still protect
//!   the admin password from a MITM, the cert is pinned by SHA-256
//!   trust-on-first-use: an unauthenticated HealthCheck runs FIRST, its peer
//!   certificate (read back via reqwest's TlsInfo) is pinned or compared,
//!   and only if it matches does the password go out -- over the same pooled
//!   connection. `forget_pin` clears the pin when a server legitimately
//!   regenerates its certificate.
//! - Response-key casing differs between the shipped docs (PascalCase) and
//!   what servers actually send (camelCase for at least some fields), so
//!   every deserialized field carries aliases for both.

use serde::Deserialize;
use sha2::Digest;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

pub const DEFAULT_PORT: u16 = 7777;

// ---------------------------------------------------------------------------
// Trust-on-first-use certificate pinning
// ---------------------------------------------------------------------------

/// Returned inside the error when a pinned certificate no longer matches, so
/// the frontend can offer to trust the new one instead of treating it as an
/// ordinary failure.
pub const PIN_MISMATCH_MARKER: &str = "TOFU_PIN_MISMATCH";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Pin store: a small JSON object { "<base_url>": "<sha256 hex>" } in the
/// app-data dir. Corrupt/missing reads behave as "nothing pinned yet".
fn load_pins(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_pin(path: &Path, key: &str, fingerprint_hex: &str) {
    let mut pins = load_pins(path);
    pins.insert(key.to_string(), fingerprint_hex.to_string());
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(&pins) {
        let _ = std::fs::write(path, text); // best-effort: next login re-pins
    }
}

/// Drop the pinned certificate for `host_input` (user confirmed the server
/// legitimately changed certs, e.g. after a reinstall).
pub fn forget_pin(pin_path: &Path, host_input: &str) -> Result<(), String> {
    let key = base_url(host_input)?;
    let mut pins = load_pins(pin_path);
    if pins.remove(&key).is_some() {
        let text = serde_json::to_string_pretty(&pins)
            .map_err(|e| format!("Pin store serialize failed: {e}"))?;
        std::fs::write(pin_path, text)
            .map_err(|e| format!("Failed to write {}: {}", pin_path.display(), e))?;
    }
    Ok(())
}

/// SHA-256 of the peer's leaf certificate for the response that just came
/// back (reqwest exposes it via TlsInfo when the client sets `tls_info(true)`).
fn peer_cert_fingerprint(resp: &reqwest::blocking::Response) -> Option<String> {
    let info = resp.extensions().get::<reqwest::tls::TlsInfo>()?;
    let der = info.peer_certificate()?;
    Some(hex(&sha2::Sha256::digest(der)))
}

/// One entry of a session's save list, as returned by `EnumerateSessions`.
/// Only the fields the fetch flow needs; unknown fields are ignored.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct SaveHeader {
    #[serde(default, alias = "saveName", alias = "SaveName")]
    pub save_name: String,
    #[serde(default, alias = "sessionName", alias = "SessionName")]
    pub session_name: String,
    /// UE timestamp, e.g. "2026.07.20-18.30.12". Zero-padded, so
    /// lexicographic order is chronological order.
    #[serde(default, alias = "saveDateTime", alias = "SaveDateTime")]
    pub save_date_time: String,
}

#[derive(Debug, Deserialize)]
pub struct Session {
    #[serde(default, alias = "sessionName", alias = "SessionName")]
    pub session_name: String,
    #[serde(default, alias = "saveHeaders", alias = "SaveHeaders")]
    pub save_headers: Vec<SaveHeader>,
}

#[derive(Debug, Deserialize)]
struct SessionsData {
    #[serde(default, alias = "Sessions")]
    sessions: Vec<Session>,
}

pub struct FetchedSave {
    pub header: SaveHeader,
    pub bytes: Vec<u8>,
}

/// Normalize what a user types into the address field to the API endpoint:
/// "my.host", "my.host:7778", "https://my.host:7778/", "[::1]:7778" all work;
/// a missing port means the game's default 7777.
pub fn base_url(input: &str) -> Result<String, String> {
    let mut rest = input.trim();
    for scheme in ["https://", "http://"] {
        if let Some(stripped) = rest.strip_prefix(scheme) {
            rest = stripped;
        }
    }
    let rest = rest.trim_matches('/');
    if rest.is_empty() {
        return Err("Enter the server's host name or IP.".to_string());
    }
    let parse_port = |p: &str| p.parse::<u16>().map_err(|_| format!("Invalid port: {p}"));
    // Bracketed IPv6 first, then host:port, then bare host. A bare IPv6
    // address (multiple ':') needs brackets to carry a port, matching URLs.
    let (host, port) = if let Some(after) = rest.strip_prefix('[') {
        let (addr, tail) = after
            .split_once(']')
            .ok_or_else(|| format!("Invalid address: {rest}"))?;
        let port = match tail.strip_prefix(':') {
            Some(p) => parse_port(p)?,
            None => DEFAULT_PORT,
        };
        (format!("[{addr}]"), port)
    } else {
        match rest.rsplit_once(':') {
            Some((h, p)) if !h.is_empty() && !h.contains(':') => (h.to_string(), parse_port(p)?),
            _ => (rest.to_string(), DEFAULT_PORT),
        }
    };
    Ok(format!("https://{host}:{port}/api/v1"))
}

fn http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        // The server cert is self-signed and X.509 v1: platform TLS accepts
        // it (rustls cannot even parse v1). The password is protected by the
        // SHA-256 cert pin checked before login, not by chain validation.
        .danger_accept_invalid_certs(true)
        // Expose the peer certificate on each response so the pin can be
        // computed/verified (see peer_cert_fingerprint).
        .tls_info(true)
        // Reuse one connection across HealthCheck -> login -> download so the
        // password rides the exact connection whose cert was just pinned.
        .pool_max_idle_per_host(4)
        .connect_timeout(Duration::from_secs(10))
        // No overall timeout -- DownloadSaveGame bodies can be hundreds of
        // MB over slow links (reqwest's blocking default would cap at 30s).
        .timeout(None)
        .build()
        .map_err(|e| format!("HTTP client init failed: {e}"))
}

/// HealthCheck (no auth) FIRST, then pin/verify its peer certificate before
/// any credential is sent. On a pin mismatch the error carries
/// PIN_MISMATCH_MARKER so the UI can offer an explicit re-trust.
fn healthcheck_and_pin(
    client: &reqwest::blocking::Client,
    base_url: &str,
    pin_path: &Path,
) -> Result<(), String> {
    let resp = call(client, base_url, None, "HealthCheck",
                    serde_json::json!({ "clientCustomData": "" }))?;
    let fingerprint = peer_cert_fingerprint(&resp)
        .ok_or("Could not read the server's TLS certificate to pin it.")?;
    // Drain/validate the HealthCheck envelope too (a non-server endpoint
    // would fail here rather than pinning a stranger's cert).
    json_data("HealthCheck", resp)?;
    let key = base_url.to_string();
    match load_pins(pin_path).get(&key) {
        Some(pinned) if *pinned == fingerprint => Ok(()),
        Some(_) => Err(format!(
            "{PIN_MISMATCH_MARKER}: The server's TLS certificate is different from the one \
             pinned on first login. If the server was reinstalled or regenerated its \
             certificate this is expected; otherwise someone may be intercepting the \
             connection."
        )),
        None => {
            save_pin(pin_path, &key, &fingerprint); // trust on first use
            Ok(())
        }
    }
}

fn call(
    client: &reqwest::blocking::Client,
    base_url: &str,
    token: Option<&str>,
    function: &str,
    data: serde_json::Value,
) -> Result<reqwest::blocking::Response, String> {
    let mut request = client
        .post(base_url)
        .json(&serde_json::json!({ "function": function, "data": data }));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    request.send().map_err(|e| {
        // e's Display already names the URL when relevant; without_url()
        // would drop the one thing that helps debug a wrong address.
        format!("Could not reach the server: {e}")
    })
}

/// Decode the JSON envelope: `{"data": ...}` on success, `{"errorCode",
/// "errorMessage"}` on failure (which the server can send with HTTP 200).
fn json_data(
    function: &str,
    resp: reqwest::blocking::Response,
) -> Result<serde_json::Value, String> {
    let status = resp.status();
    let text = resp
        .text()
        .map_err(|e| format!("{function}: failed to read response: {e}"))?;
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
        format!("{function}: server returned HTTP {status} with a non-JSON body -- is this a Satisfactory dedicated server?")
    })?;
    if let Some(code) = value.get("errorCode").and_then(|v| v.as_str()) {
        return Err(match code {
            "wrong_password" => "Wrong admin password.".to_string(),
            _ => {
                let msg = value
                    .get("errorMessage")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if msg.is_empty() {
                    format!("{function}: server error {code}")
                } else {
                    format!("{function}: {msg} ({code})")
                }
            }
        });
    }
    if !status.is_success() {
        return Err(format!("{function}: server returned HTTP {status}"));
    }
    Ok(value.get("data").cloned().unwrap_or(serde_json::Value::Null))
}

/// `PasswordLogin` at Administrator privilege (required by both
/// `EnumerateSessions` and `DownloadSaveGame`) -> bearer token.
pub fn login(
    client: &reqwest::blocking::Client,
    base_url: &str,
    password: &str,
) -> Result<String, String> {
    let resp = call(
        client,
        base_url,
        None,
        "PasswordLogin",
        serde_json::json!({
            "MinimumPrivilegeLevel": "Administrator",
            "Password": password,
        }),
    )?;
    let data = json_data("PasswordLogin", resp)?;
    ["authenticationToken", "AuthenticationToken"]
        .iter()
        .find_map(|k| data.get(k).and_then(|v| v.as_str()))
        .map(str::to_owned)
        .ok_or_else(|| "PasswordLogin: no authentication token in response".to_string())
}

pub fn enumerate_sessions(
    client: &reqwest::blocking::Client,
    base_url: &str,
    token: &str,
) -> Result<Vec<Session>, String> {
    let resp = call(
        client,
        base_url,
        Some(token),
        "EnumerateSessions",
        serde_json::json!({}),
    )?;
    let data = json_data("EnumerateSessions", resp)?;
    let sessions: SessionsData = serde_json::from_value(data)
        .map_err(|e| format!("EnumerateSessions: unexpected response shape: {e}"))?;
    Ok(sessions.sessions)
}

/// The newest save across every session on the server. Headers whose own
/// session-name field is empty inherit it from their enclosing session.
pub fn latest_save(sessions: &[Session]) -> Option<SaveHeader> {
    let mut best: Option<SaveHeader> = None;
    for session in sessions {
        for header in &session.save_headers {
            if best
                .as_ref()
                .map_or(true, |b| header.save_date_time > b.save_date_time)
            {
                let mut chosen = header.clone();
                if chosen.session_name.is_empty() {
                    chosen.session_name = session.session_name.clone();
                }
                best = Some(chosen);
            }
        }
    }
    best
}

pub fn download_save(
    client: &reqwest::blocking::Client,
    base_url: &str,
    token: &str,
    save_name: &str,
) -> Result<Vec<u8>, String> {
    let resp = call(
        client,
        base_url,
        Some(token),
        "DownloadSaveGame",
        serde_json::json!({ "SaveName": save_name }),
    )?;
    let status = resp.status();
    let is_json = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|ct| ct.to_str().ok())
        .map_or(false, |ct| ct.contains("json"));
    if is_json || !status.is_success() {
        // Errors come back as the JSON envelope; a success here would mean a
        // JSON body where save bytes belong, which is equally wrong.
        return Err(json_data("DownloadSaveGame", resp).err().unwrap_or_else(|| {
            format!("DownloadSaveGame: server sent JSON instead of a save file (HTTP {status})")
        }));
    }
    resp.bytes()
        .map(|b| b.to_vec())
        .map_err(|e| format!("DownloadSaveGame: transfer failed: {e}"))
}

/// The whole flow behind the desktop app's "Fetch latest save" button.
/// `progress` receives short stage labels for the status line.
pub fn fetch_latest(
    host_input: &str,
    password: &str,
    pin_path: &Path,
    progress: &dyn Fn(String),
) -> Result<FetchedSave, String> {
    let base_url = base_url(host_input)?;
    let client = http_client()?;
    progress("Connecting…".to_string());
    // Pin/verify the certificate on an unauthenticated HealthCheck BEFORE the
    // password is sent; the pooled connection is then reused for login.
    healthcheck_and_pin(&client, &base_url, pin_path)?;
    let token = login(&client, &base_url, password)?;
    progress("Listing saves…".to_string());
    let sessions = enumerate_sessions(&client, &base_url, &token)?;
    let header =
        latest_save(&sessions).ok_or_else(|| "The server has no saves yet.".to_string())?;
    progress(format!("Downloading {}…", header.save_name));
    let bytes = download_save(&client, &base_url, &token, &header.save_name)?;
    Ok(FetchedSave { header, bytes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_accepts_the_forms_users_type() {
        assert_eq!(base_url("my.host").unwrap(), "https://my.host:7777/api/v1");
        assert_eq!(base_url(" my.host:7778 ").unwrap(), "https://my.host:7778/api/v1");
        assert_eq!(base_url("https://my.host:7778/").unwrap(), "https://my.host:7778/api/v1");
        assert_eq!(base_url("http://10.0.0.2").unwrap(), "https://10.0.0.2:7777/api/v1");
        assert_eq!(base_url("[::1]:7778").unwrap(), "https://[::1]:7778/api/v1");
        assert_eq!(base_url("::1").unwrap(), "https://::1:7777/api/v1"); // bare IPv6: needs brackets for a port
        assert!(base_url("").is_err());
        assert!(base_url("my.host:notaport").is_err());
    }

    #[test]
    fn sessions_deserialize_from_both_documented_and_actual_casings() {
        // The shipped docs say PascalCase; live servers send camelCase.
        for body in [
            r#"{"sessions":[{"sessionName":"S","saveHeaders":[{"saveName":"a","saveDateTime":"2026.07.20-10.00.00"}]}]}"#,
            r#"{"Sessions":[{"SessionName":"S","SaveHeaders":[{"SaveName":"a","SaveDateTime":"2026.07.20-10.00.00"}]}]}"#,
        ] {
            let data: SessionsData = serde_json::from_str(body).unwrap();
            assert_eq!(data.sessions.len(), 1);
            assert_eq!(data.sessions[0].session_name, "S");
            assert_eq!(data.sessions[0].save_headers[0].save_name, "a");
        }
    }

    #[test]
    fn latest_save_picks_the_newest_across_sessions() {
        let data: SessionsData = serde_json::from_str(
            r#"{"sessions":[
                {"sessionName":"old","saveHeaders":[
                    {"saveName":"old_auto","saveDateTime":"2026.07.19-23.59.59"}]},
                {"sessionName":"live","saveHeaders":[
                    {"saveName":"manual","saveDateTime":"2026.07.20-08.00.00"},
                    {"saveName":"autosave_2","saveDateTime":"2026.07.20-12.30.00"}]}
            ]}"#,
        )
        .unwrap();
        let best = latest_save(&data.sessions).unwrap();
        assert_eq!(best.save_name, "autosave_2");
        assert_eq!(best.session_name, "live"); // inherited from the session
        assert!(latest_save(&[]).is_none());
    }
}
