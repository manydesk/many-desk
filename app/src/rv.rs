// "Connect by ID" handshake crypto, wire-compatible with the web builds
// (src/renderer/rendezvous.js and the code helpers in rtc.js).
//
//   viewer ──req (to topic(ID), key from ID)──────────────▶ host   shows Accept / Reject
//   viewer ◀──offer code + host reply topic (session key)── host   on Accept
//   viewer ──answer code (session key)────────────────────▶ host   → P2P connection
//
// The ID only locates the host and wraps the first message; everything after it uses a
// random per-request key, and each side pins the other's signing key.
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Mutex;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use rand::RngCore;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub type Key = [u8; 32];

const CODE_PREFIX: &str = "MD1-";

pub fn normalize_id(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// "123456789" → "123 456 789"
pub fn format_id(id: &str) -> String {
    let d = normalize_id(id);
    d.as_bytes().chunks(3).map(|c| std::str::from_utf8(c).unwrap_or("")).collect::<Vec<_>>().join(" ")
}

pub fn random_id() -> String {
    (100_000_000 + rand::random::<u32>() % 900_000_000).to_string()
}

pub fn topic_for_id(id: &str) -> String {
    hex::encode(Sha256::digest(format!("manydesk-topic-v1:{}", normalize_id(id)).as_bytes()))
}

pub fn random_topic() -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

/// PBKDF2-SHA256 (200 000 rounds) of the ID — slow on purpose, so cached.
pub fn key_for_id(id: &str) -> Key {
    static CACHE: Mutex<Option<HashMap<String, Key>>> = Mutex::new(None);
    let id = normalize_id(id);
    let mut g = CACHE.lock().unwrap();
    let map = g.get_or_insert_with(HashMap::new);
    if let Some(k) = map.get(&id) {
        return *k;
    }
    let mut key = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(id.as_bytes(), b"manydesk-rv-v1", 200_000, &mut key);
    map.insert(id, key);
    key
}

/// A fresh per-request key and its base64 form (sent inside the ID-wrapped request).
pub fn new_session_key() -> (Key, String) {
    let mut k = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut k);
    (k, STANDARD.encode(k))
}

pub fn import_session_key(b64: &str) -> Option<Key> {
    STANDARD.decode(b64).ok()?.try_into().ok()
}

/// AES-256-GCM: base64(iv ‖ ciphertext) of the JSON.
pub fn seal(key: &Key, obj: &Value) -> String {
    let cipher = Aes256Gcm::new(key.into());
    let mut iv = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut iv);
    let ct = cipher.encrypt(Nonce::from_slice(&iv), obj.to_string().as_bytes()).unwrap_or_default();
    let mut out = iv.to_vec();
    out.extend_from_slice(&ct);
    STANDARD.encode(out)
}

/// None if it is not for us (wrong key) or tampered.
pub fn open(key: &Key, text: &str) -> Option<Value> {
    let bytes = STANDARD.decode(text).ok()?;
    if bytes.len() < 13 {
        return None;
    }
    let pt = Aes256Gcm::new(key.into()).decrypt(Nonce::from_slice(&bytes[..12]), &bytes[12..]).ok()?;
    let v: Value = serde_json::from_slice(&pt).ok()?;
    v["t"].is_string().then_some(v)
}

/// Invite / answer codes: "MD1-" + base64url(deflate-raw(JSON)).
pub fn encode_code(obj: &Value) -> String {
    let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    let _ = z.write_all(obj.to_string().as_bytes());
    format!("{CODE_PREFIX}{}", URL_SAFE_NO_PAD.encode(z.finish().unwrap_or_default()))
}

pub fn decode_code(code: &str) -> Option<Value> {
    let clean: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    let body = clean.strip_prefix(CODE_PREFIX)?;
    let packed = URL_SAFE_NO_PAD.decode(body.trim_end_matches('=')).ok()?;
    let mut json = String::new();
    flate2::read::DeflateDecoder::new(&packed[..]).read_to_string(&mut json).ok()?;
    let v: Value = serde_json::from_str(&json).ok()?;
    (v["v"] == 1 && (v.get("sdp").is_some() || v.get("mdp").is_some())).then_some(v)
}

/// The six-digit security code both sides show, from the two certificate fingerprints.
pub fn security_code(a: &str, b: &str) -> String {
    let mut parts = [a, b];
    parts.sort();
    let h = Sha256::digest(parts.join("|").as_bytes());
    let n = ((h[0] as u32) << 16 | (h[1] as u32) << 8 | h[2] as u32) % 1_000_000;
    let s = format!("{n:06}");
    format!("{} {}", &s[..3], &s[3..])
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_roundtrip() {
        let v = serde_json::json!({ "v": 1, "t": "offer", "mdp": { "cands": ["1.2.3.4:5"], "fp": "ab", "token": "cd" } });
        assert_eq!(decode_code(&encode_code(&v)).unwrap(), v);
    }

    #[test]
    fn seal_roundtrip() {
        let (k, b64) = new_session_key();
        assert_eq!(import_session_key(&b64).unwrap(), k);
        let v = serde_json::json!({ "t": "req", "x": 1 });
        assert_eq!(open(&k, &seal(&k, &v)).unwrap(), v);
    }

    #[test]
    fn ids() {
        assert_eq!(format_id("123456789"), "123 456 789");
        assert_eq!(topic_for_id("123 456 789").len(), 64);
    }
}
