// UI translations (az, en, ru, tr), shared with the web builds via assets/i18n.json.
// t("key") returns the text in the current language, falling back to English, then
// the key; "{name}" placeholders are filled by tf().
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use serde::Deserialize;

#[derive(Deserialize)]
struct Data {
    langs: HashMap<String, String>,
    d: HashMap<String, HashMap<String, String>>,
}

fn data() -> &'static Data {
    static D: OnceLock<Data> = OnceLock::new();
    D.get_or_init(|| serde_json::from_str(include_str!("../assets/i18n.json")).expect("i18n.json"))
}

static CURRENT: RwLock<String> = RwLock::new(String::new());

/// The available languages: (code, native name), in a stable order.
pub fn languages() -> Vec<(&'static str, &'static str)> {
    ["az", "en", "ru", "tr"].iter().filter_map(|c| data().langs.get(*c).map(|n| (*c, n.as_str()))).collect()
}

/// The OS language if we have it, else English.
pub fn system() -> String {
    for loc in sys_locale::get_locales() {
        let code = loc.split(['-', '_']).next().unwrap_or("").to_lowercase();
        if data().d.contains_key(&code) {
            return code;
        }
    }
    "en".into()
}

/// "system" or a language code.
pub fn set(choice: &str) {
    let lang = if choice == "system" || !data().d.contains_key(choice) { system() } else { choice.to_string() };
    *CURRENT.write().unwrap() = lang;
}

pub fn current() -> String {
    let c = CURRENT.read().unwrap().clone();
    if c.is_empty() { system() } else { c }
}

pub fn t(key: &str) -> String {
    let d = &data().d;
    d.get(&current())
        .and_then(|m| m.get(key))
        .or_else(|| d.get("en").and_then(|m| m.get(key)))
        .cloned()
        .unwrap_or_else(|| key.to_string())
}

pub fn tf(key: &str, vars: &[(&str, &str)]) -> String {
    let mut s = t(key);
    for (k, v) in vars {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

/// For texts that carry markup in the web UI (e.g. <b>…</b>): drop the tags.
pub fn plain(key: &str) -> String {
    let s = t(key);
    let mut out = String::with_capacity(s.len());
    let mut tag = false;
    for c in s.chars() {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            _ if !tag => out.push(c),
            _ => {}
        }
    }
    out
}
