// Checking for a newer ManyDesk. The app reads the release list of the GitHub repository,
// takes the newest published (not draft, not pre-release) version that has a package for
// this platform, and offers to download it. The download is checked against the release's
// SHA256SUMS before the installer opens (the user finishes the install). Best effort — any
// network error is quiet unless the user asked for the check.
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The repository whose GitHub releases are the updates. Empty ⇒ updates are off.
pub const REPO: &str = "manydesk/many-desk";

/// Fallback when the GitHub API refuses (rate limit, proxy): every release also carries a
/// manifest (.github/workflows/build.yml), and this address always leads to the newest.
pub const MANIFEST_URL: &str = "https://github.com/manydesk/many-desk/releases/latest/download/latest.json";

#[derive(Clone)]
pub struct Update {
    pub version: String,
    #[allow(dead_code)]
    pub notes: String,
    pub url: String,
    /// The release's SHA256SUMS, when it has one.
    pub sums: Option<String>,
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

fn client(secs: u64) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(secs))
        .user_agent(concat!("ManyDesk/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())
}

async fn get_json(c: &reqwest::Client, url: &str) -> Result<Value, String> {
    let r = c.get(url).header("Accept", "application/vnd.github+json").send().await.map_err(|e| e.to_string())?;
    r.error_for_status().map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())
}

/// Is `name` this platform's package? (ManyDesk-0.20.0-mac-universal.dmg, …-win-x64.exe, …-linux-amd64.deb)
fn is_ours(name: &str) -> bool {
    let ext = if cfg!(target_os = "macos") {
        ".dmg"
    } else if cfg!(windows) {
        ".exe"
    } else {
        ".deb"
    };
    name.starts_with("ManyDesk-") && name.ends_with(&format!("-{}{ext}", asset_key()))
}

/// The newest usable release in a GitHub release list, if it is newer than `current`.
pub fn pick(list: &Value, current: &str) -> Option<Update> {
    let mut best: Option<Update> = None;
    for r in list.as_array()? {
        if r["draft"].as_bool().unwrap_or(false) || r["prerelease"].as_bool().unwrap_or(false) {
            continue;
        }
        let version = r["tag_name"].as_str().unwrap_or("").trim_start_matches('v').to_string();
        if version.is_empty() || !newer(&version, current) || best.as_ref().is_some_and(|b| !newer(&version, &b.version)) {
            continue;
        }
        let assets = r["assets"].as_array().map(|a| a.as_slice()).unwrap_or(&[]);
        let url_of = |want: &dyn Fn(&str) -> bool| {
            assets.iter().find(|a| want(a["name"].as_str().unwrap_or(""))).and_then(|a| a["browser_download_url"].as_str()).map(str::to_string)
        };
        let Some(url) = url_of(&is_ours) else { continue };
        best = Some(Update { version, notes: r["body"].as_str().unwrap_or("").to_string(), url, sums: url_of(&|n| n == "SHA256SUMS") });
    }
    best
}

/// Looks for an update newer than `current`: the GitHub release list first, the newest
/// release's manifest when the API cannot be reached.
pub async fn check(current: &str) -> Result<Option<Update>, String> {
    if REPO.is_empty() {
        return Err("no update server".into());
    }
    let c = client(20)?;
    match get_json(&c, &format!("https://api.github.com/repos/{REPO}/releases?per_page=20")).await {
        Ok(list) if list.is_array() => {
            let u = pick(&list, current);
            match &u {
                Some(u) => eprintln!("[update] {current} -> {} available: {}", u.version, u.url),
                None => eprintln!("[update] {current} is the newest for {}", asset_key()),
            }
            return Ok(u);
        }
        Ok(_) => {}
        Err(e) => eprintln!("[update] release list: {e}; trying the manifest"),
    }
    let m = get_json(&c, MANIFEST_URL).await?;
    let version = m["version"].as_str().unwrap_or("").to_string();
    if version.is_empty() || !newer(&version, current) {
        return Ok(None);
    }
    let url = m["assets"][asset_key()].as_str().unwrap_or("").to_string();
    if url.is_empty() {
        return Err("no build for this platform".into());
    }
    let sums = url.rsplit_once('/').map(|(base, _)| format!("{base}/SHA256SUMS"));
    Ok(Some(Update { version, notes: m["notes"].as_str().unwrap_or("").to_string(), url, sums }))
}

/// The hash SHA256SUMS lists for `name` (lines of "<hex>  <name>", "*<name>" in binary mode).
fn sum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (hash, file) = l.trim().split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name).then(|| hash.to_ascii_lowercase())
    })
}

/// Downloads the installer to Downloads and opens it (the OS/user does the rest).
pub async fn download_and_open(u: &Update) -> Result<std::path::PathBuf, String> {
    let dir = directories::UserDirs::new().and_then(|u| u.download_dir().map(|d| d.to_path_buf())).unwrap_or_else(std::env::temp_dir);
    let path = download(u, &dir).await?;
    open_path(&path)?;
    Ok(path)
}

/// Downloads the package into `dir` and checks it against the release's SHA256SUMS;
/// nothing is written when it does not match.
pub async fn download(u: &Update, dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let url = u.url.as_str();
    // Only https downloads from GitHub.
    if !url.starts_with("https://github.com/") && !url.starts_with("https://objects.githubusercontent.com/") {
        return Err("bad url".into());
    }
    let name = url.rsplit('/').next().filter(|n| !n.is_empty() && !n.contains("..")).ok_or("bad name")?;
    let name: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || ".-_".contains(*c)).collect();
    let path = dir.join(&name);
    let client = client(600)?;
    let bytes = client.get(url).send().await.map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?.bytes().await.map_err(|e| e.to_string())?;
    // A release with SHA256SUMS must match it; a damaged or swapped download never opens.
    if let Some(sums) = &u.sums {
        let list = client.get(sums).send().await.map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?.text().await.map_err(|e| e.to_string())?;
        let want = sum_for(&list, &name).ok_or_else(|| format!("{name} is not in SHA256SUMS"))?;
        if hex::encode(Sha256::digest(&bytes)) != want {
            return Err(format!("{name}: checksum mismatch, not saved"));
        }
    }
    tokio::fs::write(&path, &bytes).await.map_err(|e| e.to_string())?;
    eprintln!("[update] saved {} ({} bytes, checksum {})", path.display(), bytes.len(), if u.sums.is_some() { "ok" } else { "not published" });
    Ok(path)
}

fn open_path(p: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let (cmd, args): (&str, Vec<&std::ffi::OsStr>) = ("open", vec![p.as_os_str()]);
    #[cfg(windows)]
    let (cmd, args): (&str, Vec<&std::ffi::OsStr>) = ("cmd", vec!["/C".as_ref(), "start".as_ref(), "".as_ref(), p.as_os_str()]);
    #[cfg(target_os = "linux")]
    let (cmd, args): (&str, Vec<&std::ffi::OsStr>) = ("xdg-open", vec![p.as_os_str()]);
    // phones update through their stores
    #[cfg(mobile)]
    return Err(format!("open {} by hand", p.display()));
    #[cfg(not(mobile))]
    std::process::Command::new(cmd).args(args).spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_newest_published_release_for_this_platform() {
        let name = |v: &str| {
            let key = asset_key();
            let ext = if cfg!(target_os = "macos") { "dmg" } else if cfg!(windows) { "exe" } else { "deb" };
            format!("ManyDesk-{v}-{key}.{ext}")
        };
        let rel = |tag: &str, draft: bool, pre: bool, with_pkg: bool| {
            let v = tag.trim_start_matches('v');
            let mut assets = vec![serde_json::json!({"name": "SHA256SUMS", "browser_download_url": format!("https://github.com/x/{tag}/SHA256SUMS")})];
            if with_pkg {
                assets.push(serde_json::json!({"name": name(v), "browser_download_url": format!("https://github.com/x/{tag}/{}", name(v))}));
            }
            serde_json::json!({"tag_name": tag, "draft": draft, "prerelease": pre, "body": "notes", "assets": assets})
        };
        let list = serde_json::json!([
            rel("v0.23.0", true, false, true),  // draft
            rel("v0.22.0", false, true, true),  // pre-release
            rel("v0.21.1", false, false, false), // no package for us
            rel("v0.21.0", false, false, true),
            rel("v0.20.5", false, false, true),
        ]);
        let u = pick(&list, "0.20.0").expect("an update");
        assert_eq!(u.version, "0.21.0");
        assert!(u.url.ends_with(&name("0.21.0")));
        assert!(u.sums.as_deref().unwrap().ends_with("v0.21.0/SHA256SUMS"));
        assert!(pick(&list, "0.21.0").is_none());
    }

    #[test]
    fn reads_sha256sums() {
        let s = "aa11  ManyDesk-0.20.0-win-x64.exe\nBB22 *ManyDesk-0.20.0-mac-universal.dmg\n";
        assert_eq!(sum_for(s, "ManyDesk-0.20.0-win-x64.exe").as_deref(), Some("aa11"));
        assert_eq!(sum_for(s, "ManyDesk-0.20.0-mac-universal.dmg").as_deref(), Some("bb22"));
        assert_eq!(sum_for(s, "other"), None);
    }

    /// Against the real GitHub releases: an older build finds the newest release, and its
    /// package downloads and matches SHA256SUMS. `cargo test --release --lib -- --ignored live_`
    #[test]
    #[ignore = "network"]
    fn live_update_from_github() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let u = check("0.0.1").await.expect("check").expect("an update for 0.0.1");
            println!("found {} at {} (sums: {:?})", u.version, u.url, u.sums);
            assert!(u.sums.is_some(), "the release has SHA256SUMS");
            let dir = std::env::temp_dir().join(format!("manydesk-update-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = download(&u, &dir).await.expect("download and checksum");
            println!("downloaded and verified {} ({} bytes)", path.display(), std::fs::metadata(&path).unwrap().len());
            assert!(check(&u.version).await.expect("check").is_none(), "the newest version itself is up to date");
            let _ = std::fs::remove_dir_all(&dir);
        });
    }
}
