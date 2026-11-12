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

/// Windows: this copy was installed by the setup (an Uninstall.exe beside it), so it
/// updates with the setup, not the portable exe.
pub fn installed() -> bool {
    cfg!(windows) && std::env::current_exe().ok().is_some_and(|e| e.with_file_name("Uninstall.exe").exists())
}

/// Whether an update can go in without anyone answering a prompt: everywhere but an
/// installed Windows copy without the service (there the setup needs an administrator).
pub fn quiet_install_possible() -> bool {
    #[cfg(windows)]
    if installed() {
        return super::service::installed();
    }
    true
}

/// This build's download key in the manifest's "assets".
fn asset_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac-universal"
    } else if cfg!(windows) && installed() {
        "win-x64-setup"
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

/// Is `name` this platform's package? (ManyDesk-0.20.0-mac-universal.dmg, …-win-x64.exe or
/// …-win-x64-setup.exe for an installed copy, …-linux-amd64.deb)
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

/// Where updates are downloaded and staged.
fn work_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("manydesk-update")
}

/// What an install led to.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// in place: quit now, the new version starts when this process is gone
    Restart,
    /// in other hands (the Windows service, or a setup the user approved): it stops this
    /// copy and starts the new one itself
    Elsewhere,
}

/// Downloads `u`, checks it and puts it in place of the running app. `asked`: the user
/// pressed "Download and install" (an installed Windows copy without the service may then
/// ask for an administrator). On Err the package, when it got that far, comes back so the
/// caller can open it for the user to install by hand.
pub async fn install(u: &Update, #[cfg_attr(not(windows), allow(unused_variables))] asked: bool) -> Result<Outcome, (String, Option<std::path::PathBuf>)> {
    #[cfg(windows)]
    if installed() {
        if super::service::installed() {
            // the service downloads and checks the setup itself and runs it as SYSTEM
            super::service::request_update().map_err(|e| (e, None))?;
            eprintln!("[update] {}: the service installs it", u.version);
            return Ok(Outcome::Elsewhere);
        }
        if !asked {
            return Err(("an administrator has to approve this update".into(), None));
        }
    }
    let dir = work_dir();
    tokio::fs::create_dir_all(&dir).await.map_err(|e| (e.to_string(), None))?;
    let path = download(u, &dir).await.map_err(|e| (e, None))?;
    let p = path.clone();
    let r = tokio::task::spawn_blocking(move || put_in_place(&p)).await.map_err(|e| e.to_string()).and_then(|r| r);
    match r {
        Ok(o) => {
            eprintln!("[update] {} installed: {o:?}", u.version);
            Ok(o)
        }
        Err(e) => {
            eprintln!("[update] could not install by itself: {e}");
            Err((e, Some(path)))
        }
    }
}

/// The command line this copy was started with, minus our own --wait-pid, for the new one.
#[cfg_attr(any(mobile, not(any(windows, target_os = "linux", target_os = "macos"))), allow(dead_code))]
fn relaunch_args() -> Vec<std::ffi::OsString> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in std::env::args_os().skip(1) {
        if skip {
            skip = false;
        } else if a == "--wait-pid" {
            skip = true;
        } else {
            out.push(a);
        }
    }
    out
}

#[cfg_attr(windows, allow(dead_code))]
fn run(cmd: &str, args: &[&std::ffi::OsStr]) -> Result<String, String> {
    let o = std::process::Command::new(cmd).args(args).output().map_err(|e| format!("{cmd}: {e}"))?;
    if o.status.success() {
        Ok(format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    } else {
        Err(format!("{cmd}: {}", String::from_utf8_lossy(&o.stderr).trim()))
    }
}

/// macOS: the DMG's app replaces the running bundle. It must carry a valid signature for
/// the same app, and when this copy is signed with a certificate, the new one must be
/// signed by it too — so Screen Recording and Accessibility, which macOS keys to that
/// signature, stay granted. A shell helper waits for this process to end, swaps the
/// bundles (putting the old one back if the swap fails) and opens the new one.
#[cfg(all(target_os = "macos", not(mobile)))]
fn put_in_place(dmg: &std::path::Path) -> Result<Outcome, String> {
    use std::ffi::OsStr;
    let exe = std::env::current_exe().and_then(|p| p.canonicalize()).map_err(|e| e.to_string())?;
    let bundle = exe.ancestors().find(|p| p.extension() == Some(OsStr::new("app"))).ok_or("not running from an .app")?.to_path_buf();
    let parent = bundle.parent().ok_or("no folder")?.to_path_buf();
    let staged = parent.join(".ManyDesk-update.app");
    let _ = std::fs::remove_dir_all(&staged);
    let mnt = work_dir().join("mnt");
    let _ = run("hdiutil", &["detach".as_ref(), "-quiet".as_ref(), "-force".as_ref(), mnt.as_os_str()]);
    std::fs::create_dir_all(&mnt).map_err(|e| e.to_string())?;
    run("hdiutil", &["attach".as_ref(), "-nobrowse".as_ref(), "-readonly".as_ref(), "-noautoopen".as_ref(), "-quiet".as_ref(), "-mountpoint".as_ref(), mnt.as_os_str(), dmg.as_os_str()])?;
    let copied = (|| {
        let app = std::fs::read_dir(&mnt).map_err(|e| e.to_string())?.flatten().map(|e| e.path()).find(|p| p.extension() == Some(OsStr::new("app"))).ok_or("no app in the disk image")?;
        same_signer(&app, &bundle)?;
        // ditto keeps the signature, extended attributes and symlinks intact
        run("ditto", &[app.as_os_str(), staged.as_os_str()]).map_err(|e| format!("cannot write to {}: {e}", parent.display()))
    })();
    let _ = run("hdiutil", &["detach".as_ref(), "-quiet".as_ref(), "-force".as_ref(), mnt.as_os_str()]);
    copied?;
    // MANYDESK_* settings (a test copy's own config folder) go to the new copy too
    let envs: String = std::env::vars().filter(|(k, _)| k.starts_with("MANYDESK_") && k != "MANYDESK_UPDATE_CURRENT").map(|(k, v)| format!(" --env {}", sh_quote(&format!("{k}={v}")))).collect();
    let args: String = relaunch_args().iter().map(|a| format!(" {}", sh_quote(&a.to_string_lossy()))).collect();
    let script = format!(
        r#"unset MANYDESK_UPDATE_CURRENT; pid={pid}; i=0
while kill -0 $pid 2>/dev/null && [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done
old="$2.old.$$"
mv "$2" "$old" || {{ rm -rf "$1"; open -n "$2"; exit 1; }}
if mv "$1" "$2"; then rm -rf "$old"; else mv "$old" "$2"; fi
open -n{envs} "$2"{sep}{args}"#,
        pid = std::process::id(),
        sep = if args.is_empty() { "" } else { " --args" },
    );
    std::process::Command::new("/bin/sh").arg("-c").arg(script).arg("manydesk-update").arg(&staged).arg(&bundle).spawn().map_err(|e| e.to_string())?;
    Ok(Outcome::Restart)
}

#[cfg(all(target_os = "macos", not(mobile)))]
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `new` is validly signed, is the same app (bundle identifier) and, when `current` is
/// signed with a certificate, meets `current`'s designated requirement (same signer).
#[cfg(all(target_os = "macos", not(mobile)))]
fn same_signer(new: &std::path::Path, current: &std::path::Path) -> Result<(), String> {
    run("codesign", &["--verify".as_ref(), "--deep".as_ref(), "--strict".as_ref(), new.as_os_str()]).map_err(|e| format!("the new app's signature: {e}"))?;
    let ident = |p: &std::path::Path| run("codesign", &["-dv".as_ref(), p.as_os_str()]).ok().and_then(|o| o.lines().find_map(|l| l.strip_prefix("Identifier=").map(str::to_string)));
    let (a, b) = (ident(new), ident(current));
    if a.is_none() || a != b {
        return Err(format!("a different app: {a:?}, not {b:?}"));
    }
    let req = run("codesign", &["-d".as_ref(), "-r-".as_ref(), current.as_os_str()])?;
    if let Some(req) = req.lines().find_map(|l| l.trim().strip_prefix("designated => ")).filter(|r| r.contains("certificate")) {
        let req = format!("={req}");
        run("codesign", &["--verify".as_ref(), "-R".as_ref(), req.as_ref(), new.as_os_str()]).map_err(|_| "signed by someone else".to_string())?;
    }
    Ok(())
}

/// Windows: a running exe can be renamed but not overwritten. The new exe goes next to
/// it, the running one becomes <name>.old.exe (removed on the next start) and the new one
/// takes its name. The service, when it keeps this copy running, starts the new one by
/// itself; otherwise the new copy is started here and waits for this one to end.
#[cfg(windows)]
fn put_in_place(new_exe: &std::path::Path) -> Result<Outcome, String> {
    // an installed copy (no service): its setup, run as an administrator after the user's
    // approval (UAC); it stops this copy and starts the new one (/RUN)
    if installed() {
        return run_elevated(new_exe, "/S /RUN").map(|()| Outcome::Elsewhere);
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let staged = exe.with_extension("new.exe");
    let old = exe.with_extension("old.exe");
    let _ = std::fs::remove_file(&old);
    std::fs::copy(new_exe, &staged).map_err(|e| format!("cannot write to {}: {e}", exe.parent().map(|p| p.display().to_string()).unwrap_or_default()))?;
    std::fs::rename(&exe, &old).map_err(|e| {
        let _ = std::fs::remove_file(&staged);
        e.to_string()
    })?;
    if let Err(e) = std::fs::rename(&staged, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(e.to_string());
    }
    let by_service = std::env::args().any(|a| a == "--background") && super::service::installed();
    if !by_service {
        std::process::Command::new(&exe).args(relaunch_args()).env_remove("MANYDESK_UPDATE_CURRENT").arg("--wait-pid").arg(std::process::id().to_string()).spawn().map_err(|e| e.to_string())?;
    }
    Ok(Outcome::Restart)
}

/// Runs `exe` with `args` as an administrator: Windows asks the user (UAC) first.
#[cfg(windows)]
fn run_elevated(exe: &std::path::Path, args: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    let w = |s: &std::ffi::OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let (verb, file, params) = (w("runas".as_ref()), w(exe.as_os_str()), w(args.as_ref()));
    // > 32: started; otherwise refused (ERROR_CANCELLED when the user said no)
    let r = unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), params.as_ptr(), std::ptr::null(), 1) } as isize;
    if r > 32 {
        Ok(())
    } else {
        Err(format!("the setup did not start ({r})"))
    }
}

/// Linux: the .deb, installed with the administrator's password (pkexec asks). A copy
/// run from the tarball is updated by hand.
#[cfg(target_os = "linux")]
fn put_in_place(deb: &std::path::Path) -> Result<Outcome, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if !exe.starts_with("/usr/") {
        return Err("not installed from the .deb".into());
    }
    run("pkexec", &["apt-get".as_ref(), "install".as_ref(), "-y".as_ref(), "--allow-downgrades".as_ref(), deb.as_os_str()])?;
    std::process::Command::new(&exe).args(relaunch_args()).env_remove("MANYDESK_UPDATE_CURRENT").arg("--wait-pid").arg(std::process::id().to_string()).spawn().map_err(|e| e.to_string())?;
    Ok(Outcome::Restart)
}

#[cfg(any(mobile, not(any(windows, target_os = "linux", target_os = "macos"))))]
fn put_in_place(_: &std::path::Path) -> Result<Outcome, String> {
    Err("updates come from the app store".into())
}

/// Start of a new copy: wait for the one it replaces (--wait-pid <pid>) to end, so the two
/// never run at once, and clear what an update left behind.
pub fn after_update() {
    let mut args = std::env::args().skip_while(|a| a != "--wait-pid").skip(1);
    if let Some(pid) = args.next().and_then(|p| p.parse::<u32>().ok()) {
        let t0 = std::time::Instant::now();
        while alive(pid) && t0.elapsed() < std::time::Duration::from_secs(15) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    #[cfg(windows)]
    if let Ok(exe) = std::env::current_exe() {
        // still in use while the service runs the old image; it goes on a later start
        let _ = std::fs::remove_file(exe.with_extension("old.exe"));
    }
    let _ = std::fs::remove_dir_all(work_dir());
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
    unsafe {
        let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if h.is_null() {
            return false;
        }
        let running = WaitForSingleObject(h, 0) == WAIT_TIMEOUT;
        CloseHandle(h);
        running
    }
}

#[cfg(not(windows))]
fn alive(pid: u32) -> bool {
    std::process::Command::new("kill").args(["-0", &pid.to_string()]).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

/// Opens a downloaded package with the system (the user installs it).
pub fn open(p: &std::path::Path) -> Result<(), String> {
    open_path(p)
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
