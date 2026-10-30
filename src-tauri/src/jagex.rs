// Jagex account auth, mirroring what the official Jagex Launcher does:
//  1. OAuth2 + PKCE login (client com_jagex_auth_desktop_launcher) -> access/refresh/id tokens
//  2. "Consent" auth (game client id) with id_token_hint -> game id_token
//  3. POST game-session/v1/sessions {idToken} -> sessionId
//  4. GET game-session/v1/accounts -> characters
// Each Jagex account signs in through a webview with its own data directory, so cookies
// (and therefore logins) never collide between accounts.
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::oneshot;

const AUTH_BASE: &str = "https://account.jagex.com";
const LAUNCHER_CLIENT_ID: &str = "com_jagex_auth_desktop_launcher";
const LAUNCHER_REDIRECT: &str = "https://secure.runescape.com/m=weblogin/launcher-redirect";
const GAME_CLIENT_ID: &str = "1fddee4e-b100-4f4e-b2b0-097f9088f9d2";
const GAME_REDIRECT: &str = "http://localhost";
const GAME_SESSION_BASE: &str = "https://auth.runescape.com/game-session/v1";
const API_BASE: &str = "https://api.jagex.com/v1";

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    /// Unix milliseconds.
    pub expires_at: i64,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Character {
    pub account_id: String,
    pub display_name: Option<String>,
    /// Set by the user to tuck uninteresting characters away.
    #[serde(default)]
    pub hidden: bool,
    /// Everything else Jagex sends per character (membership info etc.), kept verbatim.
    #[serde(flatten, default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    #[serde(default)]
    expires_in: i64,
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

pub fn decode_jwt(token: &str) -> Result<serde_json::Value, String> {
    let payload = token.split('.').nth(1).ok_or("Malformed id_token")?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

fn params_of(raw: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(raw.as_bytes()).into_owned().collect()
}

fn is_launcher_cb(s: &str) -> bool {
    s.starts_with("jagex:")
        || (s.starts_with(LAUNCHER_REDIRECT) && (s.contains("?code=") || s.contains("&code=")))
}

fn is_local_cb(s: &str) -> bool {
    s.strip_prefix("http://localhost").is_some_and(|rest| {
        rest.trim_start_matches(|c: char| c == ':' || c.is_ascii_digit()).starts_with(['/', '#', '?'])
    })
}

/// Opens a sign-in window whose data lives in `data_dir`, and resolves with the first
/// navigation that satisfies `is_callback` (which is cancelled, so the OS never hands the
/// `jagex:` scheme to the real Jagex Launcher). Errors if the user closes the window first.
pub async fn capture_redirect(
    app: &AppHandle,
    data_dir: PathBuf,
    store_id: [u8; 16],
    url: url::Url,
    title: &str,
    is_callback: fn(&str) -> bool,
) -> Result<String, String> {
    let (tx, rx) = oneshot::channel::<String>();
    let tx = Arc::new(Mutex::new(Some(tx)));
    let nav_tx = tx.clone();

    let label = format!("login-{}", uuid::Uuid::new_v4().simple());
    let builder = WebviewWindowBuilder::new(app, label, WebviewUrl::External(url))
        .title(title)
        .inner_size(520.0, 760.0)
        .on_navigation(move |u| {
            if is_callback(u.as_str()) {
                if let Some(t) = nav_tx.lock().unwrap().take() {
                    let _ = t.send(u.to_string());
                }
                return false;
            }
            true
        });

    // WKWebView isolates storage by identifier; WebView2 / WebKitGTK by directory.
    #[cfg(target_os = "macos")]
    let builder = {
        let _ = &data_dir;
        builder.data_store_identifier(store_id)
    };
    #[cfg(not(target_os = "macos"))]
    let builder = {
        let _ = store_id;
        builder.data_directory(data_dir)
    };

    let win = builder.build().map_err(|e| e.to_string())?;
    let close_tx = tx.clone();
    win.on_window_event(move |e| {
        if matches!(e, tauri::WindowEvent::Destroyed) {
            close_tx.lock().unwrap().take();
        }
    });

    let result = tokio::time::timeout(Duration::from_secs(600), rx).await;
    let _ = win.destroy();
    match result {
        Err(_) => Err("Sign-in timed out".into()),
        Ok(Err(_)) => Err("Login window was closed".into()),
        Ok(Ok(u)) => Ok(u),
    }
}

async fn post_form(url: &str, params: &[(&str, &str)]) -> Result<TokenResponse, String> {
    let res = reqwest::Client::new().post(url).form(params).send().await.map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(format!("{url} -> HTTP {status}: {body}"));
    }
    res.json().await.map_err(|e| e.to_string())
}

fn into_tokens(old: Option<&Tokens>, r: TokenResponse) -> Result<Tokens, String> {
    let pick = |new: Option<String>, old: Option<&str>| {
        new.or_else(|| old.map(str::to_owned)).ok_or_else(|| "Token response was missing a token".to_string())
    };
    Ok(Tokens {
        access_token: pick(r.access_token, old.map(|t| t.access_token.as_str()))?,
        refresh_token: pick(r.refresh_token, old.map(|t| t.refresh_token.as_str()))?,
        id_token: pick(r.id_token, old.map(|t| t.id_token.as_str()))?,
        expires_at: now_ms() + r.expires_in * 1000,
    })
}

/// Step 1: full interactive login. Returns launcher tokens.
pub async fn login(app: &AppHandle, data_dir: PathBuf, store_id: [u8; 16]) -> Result<Tokens, String> {
    let verifier = random_token(48);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_token(12);
    let auth_url = url::Url::parse_with_params(
        &format!("{AUTH_BASE}/oauth2/auth"),
        &[
            ("auth_method", ""),
            ("login_type", ""),
            ("flow", "launcher"),
            ("response_type", "code"),
            ("client_id", LAUNCHER_CLIENT_ID),
            ("redirect_uri", LAUNCHER_REDIRECT),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("prompt", "login"),
            ("scope", "openid offline gamesso.token.create user.profile.read"),
            ("state", &state),
        ],
    )
    .map_err(|e| e.to_string())?;

    let cb = capture_redirect(app, data_dir, store_id, auth_url, "Sign in to a Jagex account", is_launcher_cb).await?;

    // jagex:code=...,state=...,intent=social_auth   or   launcher-redirect?code=...&state=...
    let raw = match cb.strip_prefix("jagex:") {
        Some(r) => r.replace(',', "&"),
        None => cb.split_once('?').map(|x| x.1.to_string()).unwrap_or_default(),
    };
    let q = params_of(raw.split('#').next().unwrap_or_default());
    if q.get("state") != Some(&state) {
        return Err("Login state mismatch".into());
    }
    let code = q.get("code").ok_or_else(|| {
        format!("Login did not return a code ({})", q.get("error").map_or("unknown error", String::as_str))
    })?;

    let res = post_form(
        &format!("{AUTH_BASE}/oauth2/token"),
        &[
            ("grant_type", "authorization_code"),
            ("client_id", LAUNCHER_CLIENT_ID),
            ("code", code),
            ("code_verifier", &verifier),
            ("redirect_uri", LAUNCHER_REDIRECT),
        ],
    )
    .await?;
    into_tokens(None, res)
}

pub async fn refresh(tokens: &Tokens) -> Result<Tokens, String> {
    let res = post_form(
        &format!("{AUTH_BASE}/oauth2/token"),
        &[
            ("grant_type", "refresh_token"),
            ("client_id", LAUNCHER_CLIENT_ID),
            ("refresh_token", &tokens.refresh_token),
        ],
    )
    .await?;
    into_tokens(Some(tokens), res)
}

/// Steps 2+3: trade the launcher id_token for a game session id.
pub async fn create_game_session(
    app: &AppHandle,
    data_dir: PathBuf,
    store_id: [u8; 16],
    id_token: &str,
) -> Result<String, String> {
    let nonce = random_token(24);
    let state = random_token(12);
    let auth_url = url::Url::parse_with_params(
        &format!("{AUTH_BASE}/oauth2/auth"),
        &[
            ("id_token_hint", id_token),
            ("nonce", &nonce),
            ("prompt", "consent"),
            ("redirect_uri", GAME_REDIRECT),
            ("response_type", "id_token code"),
            ("state", &state),
            ("client_id", GAME_CLIENT_ID),
            ("scope", "openid offline"),
        ],
    )
    .map_err(|e| e.to_string())?;

    let cb = capture_redirect(app, data_dir, store_id, auth_url, "Authorising game session", is_local_cb).await?;
    let frag = params_of(cb.split_once(['#', '?']).map_or("", |x| x.1));
    if let Some(err) = frag.get("error") {
        return Err(format!("Consent failed: {}", frag.get("error_description").unwrap_or(err)));
    }
    if frag.get("state") != Some(&state) {
        return Err("Consent state mismatch".into());
    }
    let game_id_token = frag.get("id_token").ok_or("Consent did not return an id_token")?;

    let res = reqwest::Client::new()
        .post(format!("{GAME_SESSION_BASE}/sessions"))
        .json(&serde_json::json!({ "idToken": game_id_token }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("Creating game session failed: HTTP {}", res.status()));
    }
    let body: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    body["sessionId"].as_str().map(str::to_owned).ok_or_else(|| "No sessionId in response".to_string())
}

pub async fn is_session_valid(session_id: &str) -> bool {
    reqwest::Client::new()
        .get(format!("{GAME_SESSION_BASE}/sessions"))
        .bearer_auth(session_id)
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

pub async fn get_characters(session_id: &str) -> Result<Vec<Character>, String> {
    // v2 with fetchMembership is what the official launcher calls; fall back to v1.
    let base = GAME_SESSION_BASE.trim_end_matches("/v1");
    let client = reqwest::Client::new();
    let mut last = String::new();
    for url in [format!("{base}/v2/accounts?fetchMembership=true"), format!("{base}/v1/accounts")] {
        let res = client.get(&url).bearer_auth(session_id).send().await.map_err(|e| e.to_string())?;
        if res.status().is_success() {
            return res.json().await.map_err(|e| e.to_string());
        }
        last = format!("Fetching characters failed: HTTP {}", res.status());
    }
    Err(last)
}

pub async fn get_account_name(tokens: &Tokens) -> String {
    let claims = decode_jwt(&tokens.id_token).unwrap_or_default();
    let sub = claims["sub"].as_str().unwrap_or_default();
    let fallback = || {
        ["nickname", "email", "sub"].iter().find_map(|k| claims[*k].as_str()).unwrap_or("Jagex account").to_string()
    };
    let res = reqwest::Client::new()
        .get(format!("{API_BASE}/users/{sub}/displayName"))
        .bearer_auth(&tokens.access_token)
        .send()
        .await;
    let Ok(res) = res else { return fallback() };
    if !res.status().is_success() {
        return fallback();
    }
    let Ok(j) = res.json::<serde_json::Value>().await else { return fallback() };
    match (j["displayName"].as_str(), j["discriminator"].as_str()) {
        (Some(n), Some(d)) if !n.is_empty() => format!("{n}#{d}"),
        (Some(n), _) if !n.is_empty() => n.to_string(),
        _ => fallback(),
    }
}

#[cfg(debug_assertions)]
pub fn selftest_predicates() -> [(&'static str, fn(&str) -> bool); 2] {
    [("launcher", is_launcher_cb), ("local", is_local_cb)]
}
