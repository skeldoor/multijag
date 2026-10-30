// Persistence. accounts.json holds settings plus per-account metadata; each account's tokens
// are AES-256-GCM encrypted with a master key kept in the OS keychain (Windows Credential
// Manager / macOS Keychain / Linux Secret Service). If no keychain is available (e.g. a
// headless Linux box) the key falls back to a 0600 file next to accounts.json.
use std::{collections::HashMap, fs, path::Path};

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::jagex::{Character, Tokens};

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Secrets {
    pub tokens: Option<Tokens>,
    pub session_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Account {
    pub id: String,
    pub name: String,
    /// Directory / store id for this account's isolated webview data.
    pub partition: String,
    #[serde(default)]
    pub characters: Vec<Character>,
    #[serde(default)]
    pub secrets: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Settings {
    pub client: String,
    pub paths: HashMap<String, String>,
}

pub struct Store {
    pub settings: Settings,
    pub accounts: Vec<Account>,
}

#[derive(Deserialize, Default)]
struct StoreFile {
    settings: Option<Settings>,
    #[serde(default)]
    accounts: Vec<Account>,
}

pub fn default_paths() -> HashMap<String, String> {
    let exists = |p: &str| Path::new(p).exists();
    let first = |cands: &[String]| cands.iter().find(|p| exists(p)).cloned().unwrap_or_default();

    #[cfg(target_os = "windows")]
    let (runelite, official) = {
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        (
            format!("{local}\\RuneLite\\RuneLite.exe"),
            "C:\\Program Files (x86)\\Jagex Launcher\\Games\\Old School RuneScape\\Client\\osclient.exe".to_string(),
        )
    };
    #[cfg(target_os = "macos")]
    let (runelite, official) = ("/Applications/RuneLite.app/Contents/MacOS/RuneLite".to_string(), String::new());
    #[cfg(all(unix, not(target_os = "macos")))]
    let (runelite, official) = {
        let home = std::env::var("HOME").unwrap_or_default();
        (
            first(&[
                "/usr/bin/runelite".into(),
                "/usr/local/bin/runelite".into(),
                format!("{home}/.local/bin/runelite"),
                format!("{home}/.local/share/RuneLite/RuneLite.AppImage"),
            ]),
            String::new(),
        )
    };
    let _ = first;

    HashMap::from([("runelite".to_string(), runelite), ("official".to_string(), official)])
}

impl Store {
    pub fn load(dir: &Path) -> Self {
        let file: StoreFile = fs::read_to_string(dir.join("accounts.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let mut paths = default_paths();
        let mut client = "runelite".to_string();
        if let Some(s) = file.settings {
            client = s.client;
            // Keep saved paths, but let detected defaults fill any that are empty.
            for (k, v) in s.paths {
                if !v.is_empty() {
                    paths.insert(k, v);
                }
            }
        }
        Store { settings: Settings { client, paths }, accounts: file.accounts }
    }

    pub fn save(&self, dir: &Path) {
        let json = serde_json::json!({ "settings": self.settings, "accounts": self.accounts });
        let _ = fs::create_dir_all(dir);
        let _ = fs::write(dir.join("accounts.json"), serde_json::to_string_pretty(&json).unwrap_or_default());
    }
}

fn decode_key(s: &str) -> Option<[u8; 32]> {
    STANDARD.decode(s.trim()).ok()?.try_into().ok()
}

pub fn master_key(dir: &Path) -> [u8; 32] {
    if let Ok(entry) = keyring::Entry::new("multijag", "master-key") {
        match entry.get_password() {
            Ok(p) => {
                if let Some(k) = decode_key(&p) {
                    return k;
                }
            }
            Err(keyring::Error::NoEntry) => {
                let mut k = [0u8; 32];
                rand::thread_rng().fill_bytes(&mut k);
                if entry.set_password(&STANDARD.encode(k)).is_ok() {
                    return k;
                }
            }
            Err(_) => {}
        }
    }

    let path = dir.join("master.key");
    if let Some(k) = fs::read_to_string(&path).ok().and_then(|s| decode_key(&s)) {
        return k;
    }
    let mut k = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut k);
    let _ = fs::create_dir_all(dir);
    let _ = fs::write(&path, STANDARD.encode(k));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    k
}

pub fn seal(key: &[u8; 32], secrets: &Secrets) -> String {
    let cipher = Aes256Gcm::new_from_slice(key).expect("32-byte key");
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    let plain = serde_json::to_vec(secrets).unwrap_or_default();
    let ct = cipher.encrypt(Nonce::from_slice(&nonce), plain.as_ref()).unwrap_or_default();
    STANDARD.encode([nonce.as_slice(), ct.as_slice()].concat())
}

pub fn unseal(key: &[u8; 32], blob: &str) -> Option<Secrets> {
    let bytes = STANDARD.decode(blob).ok()?;
    if bytes.len() < 13 {
        return None;
    }
    let (nonce, ct) = bytes.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let plain = cipher.decrypt(Nonce::from_slice(nonce), ct).ok()?;
    serde_json::from_slice(&plain).ok()
}
