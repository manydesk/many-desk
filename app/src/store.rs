// Persistent settings and the recent-sessions list (JSON in the user's config folder).
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct Recent {
    pub id: String,
    pub name: String,
    /// the name given here (Rename); shown instead of `name`, which the computer reports
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub alias: String,
    #[serde(default)]
    pub os: String,
    pub ts: i64,
    /// JPEG thumbnail of the last frame seen
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thumb: Vec<u8>,
    /// remembered unattended-access key for this computer (salt + derived key, hex)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<(String, String)>,
    /// that computer trusts this one: no TOTP code is asked (hex token it gave us)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<String>,
    /// its network cards and LAN address, as it told on the LAN (Wake-on-LAN)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub macs: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ip: String,
}

impl Recent {
    /// What the tile and the session tab call it: the given name, else the computer's own.
    pub fn label(&self) -> String {
        if !self.alias.is_empty() {
            self.alias.clone()
        } else if !self.name.is_empty() {
            self.name.clone()
        } else {
            crate::rv::format_id(&self.id)
        }
    }
}

/// What a viewer may do on this computer. The accept window, the session card and the
/// unattended profile all edit one of these.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(default)]
pub struct Permissions {
    pub control: bool,
    pub clipboard: bool,
    pub files: bool,
    pub sound: bool,
    pub restart: bool,
    pub privacy: bool,
    pub tunnel: bool,
    /// either side may show its camera (the other side sees it in a small window)
    pub camera: bool,
    /// a shell on this computer (Actions → Terminal)
    pub terminal: bool,
}

impl Default for Permissions {
    fn default() -> Self {
        Permissions { control: true, clipboard: true, files: true, sound: true, restart: false, privacy: false, tunnel: false, camera: true, terminal: false }
    }
}

impl Permissions {
    pub const NONE: Permissions = Permissions { control: false, clipboard: false, files: false, sound: false, restart: false, privacy: false, tunnel: false, camera: false, terminal: false };
    pub const FULL: Permissions = Permissions { control: true, clipboard: true, files: true, sound: true, restart: true, privacy: true, tunnel: true, camera: true, terminal: true };
}

/// Unattended access: a password (only a key derived from it is kept) and optionally a
/// TOTP second factor; sessions opened this way get `perms`.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
#[serde(default)]
pub struct Unattended {
    pub enabled: bool,
    /// hex: random salt and PBKDF2(password, salt)
    pub salt: String,
    pub key: String,
    /// base32 TOTP secret; empty = no second factor
    pub totp: String,
    pub perms: Permissions,
    /// devices that passed the TOTP code once and need only the password since
    pub trusted: Vec<Trusted>,
}

/// A trusted device (AnyDesk/RustDesk: "trust this device" after the second factor).
#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
#[serde(default)]
pub struct Trusted {
    /// the viewer's ManyDesk ID, its name then and when it was trusted (ms)
    pub id: String,
    pub name: String,
    pub ts: i64,
    /// hex: 32 random bytes the viewer keeps; it proves them per session (auth::trust_proof)
    pub token: String,
    /// the device's identity key then (ident.rs); empty for trusts from before 0.26
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub idk: String,
}

/// At most this many trusted devices (the oldest goes).
pub const MAX_TRUSTED: usize = 128;

/// One line of the connection log.
#[derive(Serialize, Deserialize, Clone)]
pub struct LogEntry {
    pub ts: i64,
    /// "in" (someone connected here) or "out"
    pub dir: String,
    pub id: String,
    pub name: String,
    pub secs: u64,
    #[serde(default)]
    pub unattended: bool,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Store {
    pub my_id: String,
    pub lang: String,
    pub presence: bool,
    // Before v0.13 permissions were two flags; read once into `perms`.
    #[serde(skip_serializing)]
    pub allow_control: bool,
    #[serde(skip_serializing)]
    pub allow_clipboard: bool,
    /// interactive sessions: what the accept window starts with
    pub perms: Permissions,
    pub unattended: Unattended,
    /// access control list: when on, only these IDs may connect
    pub acl_enabled: bool,
    pub acl: Vec<String>,
    /// identity keys of the IDs on the list, remembered when each first got in: the ID
    /// is what they say they are, the key proves it (ident.rs)
    pub acl_keys: std::collections::BTreeMap<String, String>,
    /// identity keys of computers we connected to, by ID, remembered the first time
    /// (ident.rs): one answering for that ID with another key is not that computer.
    /// Apart from `recent`, which keeps only the last few.
    pub known_hosts: std::collections::BTreeMap<String, String>,
    /// this install's identity secret key (hex, ident.rs); never leaves this file
    pub identity: String,
    pub log: Vec<LogEntry>,
    pub stun: Vec<String>,
    pub recent: Vec<Recent>,
    pub turn: TurnCfg,
    /// check for a newer ManyDesk on startup (and offer to install it)
    pub autoupdate: bool,
    /// phones: a finger moves the pointer like a touchpad (off: it works where it touches)
    pub touchpad: bool,
    /// answer ManyDesks on the local network (lan.rs)
    pub lan_visible: bool,
}

/// TURN relay settings (Settings → TURN), as in the web build.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
#[serde(default)]
pub struct TurnCfg {
    pub enabled: bool,
    /// "manydesk" (the shared relay, turncfg::RELAY), "cloudflare" (the user's own key) or "manual"
    pub provider: String,
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
    pub cf_key_id: String,
    pub cf_token: String,
    /// short-lived Cloudflare credentials (fetched with the key above)
    pub cf_cache: Option<CfCreds>,
}

#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
#[serde(default)]
pub struct CfCreds {
    /// whose credentials: "manydesk" or the Cloudflare key ID (older caches: empty)
    pub source: String,
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
    pub expires: i64,
}

pub const DEFAULT_STUN: &[&str] = &[
    "stun:stun.l.google.com:19302",
    "stun:stun1.l.google.com:19302",
    "stun:stun.cloudflare.com:3478",
    "stun:global.stun.twilio.com:3478",
    "stun:stun.nextcloud.com:443",
    "stun:stun.sipgate.net:3478",
];

impl Default for Store {
    fn default() -> Self {
        Store {
            my_id: String::new(),
            lang: "system".into(),
            presence: true,
            allow_control: true,
            allow_clipboard: true,
            perms: Permissions::default(),
            unattended: Unattended { perms: Permissions::FULL, ..Default::default() },
            acl_enabled: false,
            acl: Vec::new(),
            acl_keys: Default::default(),
            known_hosts: Default::default(),
            identity: String::new(),
            log: Vec::new(),
            stun: DEFAULT_STUN.iter().map(|s| s.to_string()).collect(),
            recent: Vec::new(),
            turn: TurnCfg { enabled: true, provider: "manydesk".into(), ..Default::default() },
            autoupdate: true,
            touchpad: true,
            lan_visible: true,
        }
    }
}

/// The folder of this copy's settings (and ID).
pub fn config_dir() -> PathBuf {
    path().parent().map(|p| p.to_path_buf()).unwrap_or_default()
}

fn path() -> PathBuf {
    // Test hook: MANYDESK_CONFIG_DIR=<dir> keeps a test copy's settings (and ID) apart from
    // the installed app's (Windows has no HOME to point elsewhere).
    if let Some(dir) = std::env::var_os("MANYDESK_CONFIG_DIR").map(PathBuf::from) {
        let _ = std::fs::create_dir_all(&dir);
        return dir.join("settings.json");
    }
    let dir = match directories::ProjectDirs::from("az", "manydesk", "ManyDesk") {
        Some(d) => {
            let dir = d.config_dir().to_path_buf();
            owner_only(&dir);
            dir
        }
        // No home folder: a folder of our own in the temp dir (not one another user made)
        None => {
            let dir = std::env::temp_dir().join("manydesk");
            if owner_only(&dir) {
                dir
            } else {
                fresh_private_dir("manydesk")
            }
        }
    };
    dir.join("settings.json")
}

/// Makes `dir` (creating it) a folder only this user can use: the settings hold the
/// unattended key, which works like the password. False when it is not ours (another
/// user made it first, or it is a link).
fn owner_only(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir);
        let Ok(m) = std::fs::symlink_metadata(dir) else { return false };
        if !m.is_dir() {
            return false;
        }
        // ours: a file we create in it gets our uid, the same as the folder's
        let probe = dir.join(".owner-check");
        let _ = std::fs::remove_file(&probe);
        let mine = std::fs::OpenOptions::new().write(true).create_new(true).open(&probe).and_then(|f| f.metadata()).map(|f| f.uid());
        let _ = std::fs::remove_file(&probe);
        if mine.ok() != Some(m.uid()) {
            return false;
        }
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        true
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir).is_ok()
    }
}

/// A new folder in the temp dir that nobody else can have made (created exclusively).
fn fresh_private_dir(name: &str) -> PathBuf {
    loop {
        let dir = std::env::temp_dir().join(format!("{name}-{}", hex::encode(rand::random::<[u8; 8]>())));
        #[cfg(unix)]
        let made = {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new().mode(0o700).create(&dir)
        };
        #[cfg(not(unix))]
        let made = std::fs::create_dir(&dir);
        match made {
            Ok(()) => return dir,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return dir,
        }
    }
}

/// A folder of ManyDesk's own for working files (clipboard pictures, updates), only this
/// user can use: never a fixed name in the shared temp dir.
pub fn private_dir(name: &str) -> PathBuf {
    let dir = config_dir().join(name);
    if owner_only(&dir) {
        dir
    } else {
        fresh_private_dir(&format!("manydesk-{name}"))
    }
}

impl Store {
    pub fn load() -> Self {
        let raw = std::fs::read(path()).ok();
        // A UTF-8 BOM (e.g. after editing in Notepad) is fine.
        let body = raw.as_deref().map(|b| b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(b));
        let mut s: Store = match body.map(serde_json::from_slice::<Store>) {
            Some(Ok(s)) => s,
            Some(Err(e)) => {
                // Never silently replace a file we cannot read (it holds the ID): keep a copy.
                eprintln!("[store] settings.json unreadable ({e}); kept as settings.json.bad");
                let _ = std::fs::copy(path(), path().with_extension("json.bad"));
                Store::default()
            }
            None => Store::default(),
        };
        if s.my_id.len() != 9 || !s.my_id.chars().all(|c| c.is_ascii_digit()) {
            s.my_id = crate::rv::random_id();
            s.save();
        }
        if s.identity.is_empty() {
            s.identity = crate::ident::generate();
            s.save();
        }
        crate::ident::init(&s.identity);
        if s.stun.is_empty() {
            s.stun = Store::default().stun;
        }
        // TURN never set up (no key, no address): the ManyDesk relay, on, as on a new install.
        let t = &s.turn;
        if t.provider != "manydesk" && t.cf_key_id.is_empty() && t.cf_token.is_empty() && t.urls.is_empty() {
            s.turn = Store::default().turn;
        }
        s.perms.control &= s.allow_control;
        s.perms.clipboard &= s.allow_clipboard;
        (s.allow_control, s.allow_clipboard) = (true, true);
        s
    }

    pub fn save(&self) {
        // the sign-in-screen copy (SYSTEM) only reads the owner's settings
        if crate::signin() {
            return;
        }
        if let Ok(b) = serde_json::to_vec_pretty(self) {
            let p = path();
            let tmp = p.with_extension("tmp");
            let _ = std::fs::remove_file(&tmp);
            // only this user may read it: the unattended key works like the password
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
            let written = o.open(&tmp).and_then(|mut f| std::io::Write::write_all(&mut f, &b));
            if written.is_ok() {
                let _ = std::fs::rename(tmp, p);
            }
        }
    }

    /// At most this many remembered identity keys (the list is cleared of the oldest
    /// IDs only if it ever gets this long, which takes years of new computers).
    pub const MAX_KNOWN: usize = 4096;

    /// Remember a computer's identity key (first connection, or after Forget).
    pub fn pin_host(&mut self, id: &str, idk: &str) {
        if self.known_hosts.get(id).map(String::as_str) != Some(idk) {
            if self.known_hosts.len() >= Self::MAX_KNOWN && !self.known_hosts.contains_key(id) {
                let keep: std::collections::HashSet<&str> = self.recent.iter().map(|r| r.id.as_str()).collect();
                let drop: Vec<String> = self.known_hosts.keys().filter(|k| !keep.contains(k.as_str())).take(256).cloned().collect();
                for k in drop {
                    self.known_hosts.remove(&k);
                }
            }
            self.known_hosts.insert(id.to_string(), idk.to_string());
            self.save();
        }
    }

    /// Most recent first, at most 12.
    pub fn remember(&mut self, id: &str, name: &str, os: &str) {
        let prev = self.recent.iter().position(|r| r.id == id).map(|i| self.recent.remove(i));
        let (alias, thumb, auth, trust, macs, ip) = prev.map(|r| (r.alias, r.thumb, r.auth, r.trust, r.macs, r.ip)).unwrap_or_default();
        self.recent.insert(0, Recent { id: id.into(), name: name.into(), alias, os: os.into(), ts: crate::rv::now_ms(), thumb, auth, trust, macs, ip });
        self.recent.truncate(12);
        self.save();
    }

    /// Rename: an empty name goes back to the computer's own.
    pub fn set_alias(&mut self, id: &str, alias: &str) {
        if let Some(r) = self.recent.iter_mut().find(|r| r.id == id) {
            r.alias = alias.trim().chars().take(60).collect();
            self.save();
        }
    }

    /// False when that computer is not in the list.
    pub fn set_thumb(&mut self, id: &str, jpeg: Vec<u8>) -> bool {
        let Some(r) = self.recent.iter_mut().find(|r| r.id == id) else { return false };
        r.thumb = jpeg;
        self.save();
        true
    }

    /// A finished session for the connection log (newest first, at most 300).
    pub fn log(&mut self, e: LogEntry) {
        self.log.insert(0, e);
        self.log.truncate(300);
        self.save();
    }

    pub fn forget(&mut self, id: &str) {
        self.recent.retain(|r| r.id != id);
        self.save();
    }
}

/// Where received files go.
pub fn downloads() -> PathBuf {
    // Android: Download/ManyDesk in the shared storage (the app's own folder without access)
    #[cfg(target_os = "android")]
    {
        let dir = crate::android::shared_storage().map(|p| p.join("Download").join("ManyDesk")).unwrap_or_else(|| std::env::temp_dir().join("received"));
        let _ = std::fs::create_dir_all(&dir);
        return dir;
    }
    #[allow(unreachable_code)]
    // no Downloads folder set up (XDG): ~/Downloads, not the shared temp dir
    directories::UserDirs::new()
        .and_then(|u| u.download_dir().map(|d| d.to_path_buf()).or_else(|| Some(u.home_dir().join("Downloads"))))
        .map(|d| {
            let _ = std::fs::create_dir_all(&d);
            d
        })
        .unwrap_or_else(|| private_dir("received"))
}

#[cfg(all(test, unix))]
mod perm_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn private_folders_are_owner_only() {
        let dir = std::env::temp_dir().join(format!("md-private-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(owner_only(&dir));
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        // a link is not taken as our folder
        let link = std::env::temp_dir().join(format!("md-private-link-{}", std::process::id()));
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(!owner_only(&link));
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&dir);
        let f = fresh_private_dir("md-fresh");
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o700);
        let _ = std::fs::remove_dir_all(&f);
    }
}
