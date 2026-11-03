// Persistent settings and the recent-sessions list (JSON in the user's config folder).
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct Recent {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub os: String,
    pub ts: i64,
    /// JPEG thumbnail of the last frame seen
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thumb: Vec<u8>,
    /// remembered unattended-access key for this computer (salt + derived key, hex)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<(String, String)>,
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
}

impl Default for Permissions {
    fn default() -> Self {
        Permissions { control: true, clipboard: true, files: true, sound: true, restart: false, privacy: false, tunnel: false, camera: true }
    }
}

impl Permissions {
    pub const NONE: Permissions = Permissions { control: false, clipboard: false, files: false, sound: false, restart: false, privacy: false, tunnel: false, camera: false };
    pub const FULL: Permissions = Permissions { control: true, clipboard: true, files: true, sound: true, restart: true, privacy: true, tunnel: true, camera: true };
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
}

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
    pub log: Vec<LogEntry>,
    pub stun: Vec<String>,
    pub recent: Vec<Recent>,
    pub turn: TurnCfg,
    /// check for a newer ManyDesk on startup (and offer to install it)
    pub autoupdate: bool,
    /// phones: a finger moves the pointer like a touchpad (off: it works where it touches)
    pub touchpad: bool,
}

/// TURN relay settings (Settings → TURN), as in the web build.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
#[serde(default)]
pub struct TurnCfg {
    pub enabled: bool,
    /// "cloudflare" or "manual"
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
pub struct CfCreds {
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
            log: Vec::new(),
            stun: DEFAULT_STUN.iter().map(|s| s.to_string()).collect(),
            recent: Vec::new(),
            turn: TurnCfg { provider: "cloudflare".into(), ..Default::default() },
            autoupdate: true,
            touchpad: true,
        }
    }
}

fn path() -> PathBuf {
    let dir = directories::ProjectDirs::from("az", "manydesk", "ManyDesk")
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("manydesk"));
    let _ = std::fs::create_dir_all(&dir);
    dir.join("settings.json")
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
        if s.stun.is_empty() {
            s.stun = Store::default().stun;
        }
        s.perms.control &= s.allow_control;
        s.perms.clipboard &= s.allow_clipboard;
        (s.allow_control, s.allow_clipboard) = (true, true);
        s
    }

    pub fn save(&self) {
        if let Ok(b) = serde_json::to_vec_pretty(self) {
            let p = path();
            let tmp = p.with_extension("tmp");
            if std::fs::write(&tmp, b).is_ok() {
                let _ = std::fs::rename(tmp, p);
            }
        }
    }

    /// Most recent first, at most 12.
    pub fn remember(&mut self, id: &str, name: &str, os: &str) {
        let prev = self.recent.iter().position(|r| r.id == id).map(|i| self.recent.remove(i));
        let (thumb, auth) = prev.map(|r| (r.thumb, r.auth)).unwrap_or_default();
        self.recent.insert(0, Recent { id: id.into(), name: name.into(), os: os.into(), ts: crate::rv::now_ms(), thumb, auth });
        self.recent.truncate(12);
        self.save();
    }

    pub fn set_thumb(&mut self, id: &str, jpeg: Vec<u8>) {
        if let Some(r) = self.recent.iter_mut().find(|r| r.id == id) {
            r.thumb = jpeg;
            self.save();
        }
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
    directories::UserDirs::new()
        .and_then(|u| u.download_dir().map(|d| d.to_path_buf()))
        .unwrap_or_else(std::env::temp_dir)
}
