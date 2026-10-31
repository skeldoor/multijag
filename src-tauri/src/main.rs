#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod jagex;
mod store;

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use jagex::Character;
use store::{master_key, seal, unseal, Account, Secrets, Settings, Store};

/// GitHub repository the in-app update check looks at (latest published release).
const REPO: &str = "Skeldoor/multijag";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateInfo {
    current: String,
    latest: String,
    url: String,
    available: bool,
}

struct AppState {
    store: Mutex<Store>,
    key: [u8; 32],
    dir: PathBuf,
    running: Mutex<HashMap<u32, RunningView>>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RunningView {
    pid: u32,
    account_id: String,
    character_id: String,
    name: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct AccountView {
    id: String,
    name: String,
    characters: Vec<Character>,
}

// What the UI sees: never tokens.
#[derive(Serialize, Clone)]
struct View {
    version: &'static str,
    settings: Settings,
    accounts: Vec<AccountView>,
    running: Vec<RunningView>,
}

fn view(st: &AppState) -> View {
    let store = st.store.lock().unwrap();
    View {
        version: env!("CARGO_PKG_VERSION"),
        settings: store.settings.clone(),
        accounts: store
            .accounts
            .iter()
            .map(|a| AccountView { id: a.id.clone(), name: a.name.clone(), characters: a.characters.clone() })
            .collect(),
        running: st.running.lock().unwrap().values().cloned().collect(),
    }
}

fn emit(app: &AppHandle) {
    let st = app.state::<AppState>();
    let _ = app.emit("state", view(&st));
}

fn webview_dir(st: &AppState, partition: &str) -> PathBuf {
    st.dir.join("webviews").join(partition)
}

fn store_id(partition: &str) -> [u8; 16] {
    uuid::Uuid::parse_str(partition).map(|u| *u.as_bytes()).unwrap_or([0; 16])
}

fn remove_webview_data(st: &AppState, partition: &str) {
    let _ = fs::remove_dir_all(webview_dir(st, partition));
}

fn with_account<T>(st: &AppState, id: &str, f: impl FnOnce(&mut Account) -> T) -> Result<T, String> {
    let mut store = st.store.lock().unwrap();
    let acct = store.accounts.iter_mut().find(|a| a.id == id).ok_or("Unknown account")?;
    let out = f(acct);
    store.save(&st.dir);
    Ok(out)
}

fn read_secrets(st: &AppState, id: &str) -> Result<Secrets, String> {
    let store = st.store.lock().unwrap();
    let acct = store.accounts.iter().find(|a| a.id == id).ok_or("Unknown account")?;
    Ok(acct.secrets.as_deref().and_then(|b| unseal(&st.key, b)).unwrap_or_default())
}

fn write_secrets(st: &AppState, id: &str, secrets: &Secrets) -> Result<(), String> {
    with_account(st, id, |a| a.secrets = Some(seal(&st.key, secrets)))
}

async fn ensure_tokens(app: &AppHandle, id: &str) -> Result<Secrets, String> {
    let st = app.state::<AppState>();
    let mut s = read_secrets(&st, id)?;
    let t = s.tokens.clone().ok_or("Account has no tokens; remove and re-add it")?;
    if jagex::now_ms() > t.expires_at - 60_000 {
        s.tokens = Some(jagex::refresh(&t).await?);
        write_secrets(&st, id, &s)?;
    }
    Ok(s)
}

async fn ensure_session(app: &AppHandle, id: &str) -> Result<String, String> {
    let st = app.state::<AppState>();
    if let Some(sid) = read_secrets(&st, id)?.session_id {
        if jagex::is_session_valid(&sid).await {
            return Ok(sid);
        }
    }
    let mut s = ensure_tokens(app, id).await?;
    let partition = with_account(&st, id, |a| a.partition.clone())?;
    let id_token = s.tokens.as_ref().map(|t| t.id_token.clone()).unwrap_or_default();
    let sid = jagex::create_game_session(app, webview_dir(&st, &partition), store_id(&partition), &id_token).await?;
    s.session_id = Some(sid.clone());
    write_secrets(&st, id, &s)?;
    Ok(sid)
}

#[tauri::command]
fn get_state(app: AppHandle) -> View {
    view(&app.state::<AppState>())
}

#[tauri::command]
async fn add_account(app: AppHandle) -> Result<(), String> {
    let st = app.state::<AppState>();
    // Fresh webview data per sign-in: separate cookies = separate simultaneous logins.
    let partition = uuid::Uuid::new_v4().to_string();
    let result = async {
        let tokens = jagex::login(&app, webview_dir(&st, &partition), store_id(&partition)).await?;
        let sub = jagex::decode_jwt(&tokens.id_token)?["sub"].as_str().ok_or("id_token has no sub")?.to_string();
        let name = jagex::get_account_name(&tokens).await;
        let secrets = Secrets { tokens: Some(tokens), session_id: None };
        {
            let mut store = st.store.lock().unwrap();
            let sealed = Some(seal(&st.key, &secrets));
            if let Some(existing) = store.accounts.iter_mut().find(|a| a.id == sub) {
                // Re-login of an already-added account: replace its tokens and webview data.
                remove_webview_data(&st, &existing.partition);
                existing.partition = partition.clone();
                existing.name = name;
                existing.secrets = sealed;
            } else {
                store.accounts.push(Account {
                    id: sub.clone(),
                    name,
                    partition: partition.clone(),
                    characters: vec![],
                    secrets: sealed,
                });
            }
            store.save(&st.dir);
        }
        emit(&app);
        Ok::<_, String>(sub)
    }
    .await;

    match result {
        Ok(sub) => refresh_characters(app, sub).await,
        Err(e) => {
            let used = st.store.lock().unwrap().accounts.iter().any(|a| a.partition == partition);
            if !used {
                remove_webview_data(&st, &partition);
            }
            Err(e)
        }
    }
}

#[tauri::command]
async fn refresh_characters(app: AppHandle, id: String) -> Result<(), String> {
    let sid = ensure_session(&app, &id).await?;
    let mut characters = jagex::get_characters(&sid).await?;
    let st = app.state::<AppState>();
    with_account(&st, &id, |a| {
        // Keep the user's custom order; characters not seen before go last.
        let pos = |c: &Character| a.characters.iter().position(|o| o.account_id == c.account_id).unwrap_or(usize::MAX);
        characters.sort_by_key(pos);
        for c in &mut characters {
            c.hidden = a.characters.iter().any(|o| o.account_id == c.account_id && o.hidden);
        }
        a.characters = characters;
    })?;
    emit(&app);
    Ok(())
}

#[tauri::command]
fn remove_account(app: AppHandle, id: String) {
    let st = app.state::<AppState>();
    {
        let mut store = st.store.lock().unwrap();
        if let Some(pos) = store.accounts.iter().position(|a| a.id == id) {
            let acct = store.accounts.remove(pos);
            remove_webview_data(&st, &acct.partition);
            store.save(&st.dir);
        }
    }
    emit(&app);
}

#[tauri::command]
async fn launch(app: AppHandle, id: String, character_id: String) -> Result<(), String> {
    let st = app.state::<AppState>();
    let (character, exe) = {
        let store = st.store.lock().unwrap();
        let acct = store.accounts.iter().find(|a| a.id == id).ok_or("Unknown account")?;
        let character = acct
            .characters
            .iter()
            .find(|c| c.account_id == character_id)
            .cloned()
            .ok_or("Unknown character; try refreshing")?;
        let exe = store.settings.paths.get(&store.settings.client).cloned().unwrap_or_default();
        (character, exe)
    };
    if exe.is_empty() || !Path::new(&exe).exists() {
        return Err(format!("Game client not found{}. Set it under Client paths.", if exe.is_empty() { String::new() } else { format!(": {exe}") }));
    }

    let sid = ensure_session(&app, &id).await?;
    let mut cmd = Command::new(&exe);
    cmd.env("JX_SESSION_ID", sid)
        .env("JX_CHARACTER_ID", &character.account_id)
        .env_remove("JX_ACCESS_TOKEN")
        .env_remove("JX_REFRESH_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match &character.display_name {
        Some(n) => cmd.env("JX_DISPLAY_NAME", n),
        None => cmd.env_remove("JX_DISPLAY_NAME"),
    };
    if let Some(dir) = Path::new(&exe).parent() {
        cmd.current_dir(dir);
    }
    let mut child = cmd.spawn().map_err(|e| format!("Could not start {exe}: {e}"))?;

    let pid = child.id();
    st.running.lock().unwrap().insert(
        pid,
        RunningView {
            pid,
            account_id: id,
            character_id,
            name: character.display_name.unwrap_or(character.account_id),
        },
    );
    emit(&app);

    let app2 = app.clone();
    std::thread::spawn(move || {
        let _ = child.wait();
        app2.state::<AppState>().running.lock().unwrap().remove(&pid);
        emit(&app2);
    });
    Ok(())
}

#[tauri::command]
fn reorder_accounts(app: AppHandle, ids: Vec<String>) {
    let st = app.state::<AppState>();
    {
        let mut store = st.store.lock().unwrap();
        store.accounts.sort_by_key(|a| ids.iter().position(|i| *i == a.id).unwrap_or(usize::MAX));
        store.save(&st.dir);
    }
    emit(&app);
}

#[tauri::command]
fn reorder_characters(app: AppHandle, id: String, ids: Vec<String>) -> Result<(), String> {
    let st = app.state::<AppState>();
    with_account(&st, &id, |a| {
        a.characters.sort_by_key(|c| ids.iter().position(|i| *i == c.account_id).unwrap_or(usize::MAX));
    })?;
    emit(&app);
    Ok(())
}

#[tauri::command]
fn set_hidden(app: AppHandle, id: String, character_id: String, hidden: bool) -> Result<(), String> {
    let st = app.state::<AppState>();
    with_account(&st, &id, |a| {
        if let Some(c) = a.characters.iter_mut().find(|c| c.account_id == character_id) {
            c.hidden = hidden;
        }
    })?;
    emit(&app);
    Ok(())
}

fn parse_version(v: &str) -> Vec<u64> {
    v.trim_start_matches(['v', 'V']).split(['.', '-', '+']).map_while(|p| p.parse().ok()).collect()
}

#[tauri::command]
async fn check_update() -> Result<UpdateInfo, String> {
    let res = reqwest::Client::new()
        .get(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .header("User-Agent", "MultiJag")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("Could not reach GitHub: {e}"))?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Err("No published release found yet (or the repository is private)".into());
    }
    if !res.status().is_success() {
        return Err(format!("GitHub returned HTTP {}", res.status()));
    }
    let body: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    let latest = body["tag_name"].as_str().ok_or("Release has no tag")?.to_string();
    let current = env!("CARGO_PKG_VERSION").to_string();
    Ok(UpdateInfo {
        available: parse_version(&latest) > parse_version(&current),
        url: body["html_url"].as_str().unwrap_or_default().to_string(),
        latest,
        current,
    })
}

#[tauri::command]
fn open_link(app: AppHandle, url: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    // Only ever open our own repo and Jagex's account / legal pages.
    let allowed = [
        format!("https://github.com/{REPO}/"),
        "https://account.jagex.com/".to_string(),
        "https://legal.jagex.com/".to_string(),
    ];
    let lower = url.to_ascii_lowercase();
    if !allowed.iter().any(|p| lower.starts_with(&p.to_ascii_lowercase())) {
        return Err("Refusing to open an unexpected link".into());
    }
    app.opener().open_url(url, None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_client(app: AppHandle, client: String) {
    let st = app.state::<AppState>();
    {
        let mut store = st.store.lock().unwrap();
        store.settings.client = client;
        store.save(&st.dir);
    }
    emit(&app);
}

#[tauri::command]
async fn browse_client(app: AppHandle, client: String) -> Result<(), String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().set_title("Select the game client executable").pick_file(move |p| {
        let _ = tx.send(p);
    });
    if let Some(picked) = rx.await.map_err(|e| e.to_string())? {
        let path = picked.into_path().map_err(|e| e.to_string())?;
        let st = app.state::<AppState>();
        {
            let mut store = st.store.lock().unwrap();
            store.settings.paths.insert(client, path.to_string_lossy().into_owned());
            store.save(&st.dir);
        }
        emit(&app);
    }
    Ok(())
}

// Debug builds only: MULTIJAG_CAPTURE_TEST=http://127.0.0.1:PORT runs the redirect-capture
// logic against a local fake server (/302jagex, /jsjagex, /302local) and exits.
#[cfg(debug_assertions)]
fn capture_selftest(app: AppHandle, base: String) {
    tauri::async_runtime::spawn(async move {
        let cases = [("/302jagex", 0), ("/jsjagex", 0), ("/302local", 1)];
        let preds = jagex::selftest_predicates();
        let dir = std::env::temp_dir().join("multijag-capture-test");
        for (path, p) in cases {
            let url = url::Url::parse(&format!("{base}{path}")).unwrap();
            let r = jagex::capture_redirect(&app, dir.clone(), [7; 16], url, "test", preds[p].1).await;
            println!("RESULT {path} -> {r:?}");
        }
        app.exit(0);
    });
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            fs::create_dir_all(&dir)?;
            app.manage(AppState {
                key: master_key(&dir),
                store: Mutex::new(Store::load(&dir)),
                dir,
                running: Mutex::new(HashMap::new()),
            });
            #[cfg(debug_assertions)]
            if let Ok(base) = std::env::var("MULTIJAG_CAPTURE_TEST") {
                capture_selftest(app.handle().clone(), base);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            add_account,
            remove_account,
            refresh_characters,
            launch,
            reorder_accounts,
            reorder_characters,
            set_hidden,
            check_update,
            open_link,
            set_client,
            browse_client
        ])
        .run(tauri::generate_context!())
        .expect("error while running MultiJag");
}
