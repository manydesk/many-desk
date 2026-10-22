// Checking for a newer ManyDesk. A small JSON manifest lists the latest version and a
// download per platform; when it is newer the app offers to fetch it and open the
// installer (the user finishes the install). Best effort — any network error is quiet
// unless the user asked for the check.
use serde_json::Value;

/// Where the manifest lives. Empty ⇒ updates are off (no server configured yet).
pub const MANIFEST_URL: &str = "";

pub struct Update {
    pub version: String,
    #[allow(dead_code)]
    pub notes: String,
    pub url: String,
}

/// This build's download key in the manifest's "assets".
fn asset_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac-universal"
    } else if cfg!(windows) {
        "win-x64"
    } else if cfg!(target_arch = "aarch64") {
        "linux-arm64"
    } else {
        "linux-amd64"
    }
}

/// "0.14.0" > "0.13.0"? Compares dotted numbers, missing parts as 0.
pub fn newer(remote: &str, local: &str) -> bool {
    let parse = |s: &str| s.split('.').map(|p| p.trim().parse::<u32>().unwrap_or(0)).collect::<Vec<_>>();
    let (a, b) = (parse(remote), parse(local));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

/// Fetches the manifest and returns an update when one is newer than `current`.
pub async fn check(current: &str) -> Result<Option<Update>, String> {
    if MANIFEST_URL.is_empty() {
        return Err("no update server".into());
    }
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).build().map_err(|e| e.to_string())?;
    let m: Value = client.get(MANIFEST_URL).send().await.map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
    let version = m["version"].as_str().unwrap_or("").to_string();
    if version.is_empty() || !newer(&version, current) {
        return Ok(None);
    }
    let url = m["assets"][asset_key()].as_str().unwrap_or("").to_string();
    if url.is_empty() {
        return Err("no build for this platform".into());
    }
    Ok(Some(Update { version, notes: m["notes"].as_str().unwrap_or("").to_string(), url }))
}

/// Downloads the installer to Downloads and opens it (the OS/user does the rest).
pub async fn download_and_open(url: &str) -> Result<std::path::PathBuf, String> {
    // Only our own https downloads.
    if !url.starts_with("https://") {
        return Err("bad url".into());
    }
    let name = url.rsplit('/').next().filter(|n| !n.is_empty() && !n.contains("..")).ok_or("bad name")?;
    let name: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || ".-_".contains(*c)).collect();
    let dir = directories::UserDirs::new().and_then(|u| u.download_dir().map(|d| d.to_path_buf())).unwrap_or_else(std::env::temp_dir);
    let path = dir.join(&name);
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(600)).build().map_err(|e| e.to_string())?;
    let bytes = client.get(url).send().await.map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?.bytes().await.map_err(|e| e.to_string())?;
    tokio::fs::write(&path, &bytes).await.map_err(|e| e.to_string())?;
    open_path(&path)?;
    Ok(path)
}

fn open_path(p: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let (cmd, args): (&str, Vec<&std::ffi::OsStr>) = ("open", vec![p.as_os_str()]);
    #[cfg(windows)]
    let (cmd, args): (&str, Vec<&std::ffi::OsStr>) = ("cmd", vec!["/C".as_ref(), "start".as_ref(), "".as_ref(), p.as_os_str()]);
    #[cfg(target_os = "linux")]
    let (cmd, args): (&str, Vec<&std::ffi::OsStr>) = ("xdg-open", vec![p.as_os_str()]);
    std::process::Command::new(cmd).args(args).spawn().map(|_| ()).map_err(|e| e.to_string())
}
