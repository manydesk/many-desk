// Unattended access. The host keeps only K = PBKDF2-SHA256(password, salt); when a
// session opens it sends a nonce and the salt, and the viewer answers with
//   HMAC-SHA256(K, "manydesk-auth-v1" | nonce | host fp | viewer fp [| TOTP code])
// so neither the password nor the code crosses the network, and a man in the middle who
// swapped TLS certificates cannot produce a proof that matches the real host's view.
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::Sha256;

const ROUNDS: u32 = 150_000;

pub fn new_salt() -> String {
    hex::encode(rand::random::<[u8; 16]>())
}

/// K for a password (slow on purpose).
pub fn derive(password: &str, salt_hex: &str) -> [u8; 32] {
    let salt = hex::decode(salt_hex).unwrap_or_default();
    let mut k = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, ROUNDS, &mut k);
    k
}

fn mac(key: &[u8], nonce: &str, host_fp: &str, viewer_fp: &str, code: Option<u32>) -> Hmac<Sha256> {
    let mut mac = <Hmac<Sha256>>::new_from_slice(key).expect("hmac key");
    mac.update(b"manydesk-auth-v1");
    for part in [nonce, host_fp, viewer_fp] {
        mac.update(&(part.len() as u32).to_be_bytes());
        mac.update(part.as_bytes());
    }
    if let Some(c) = code {
        mac.update(format!("{c:06}").as_bytes());
    }
    mac
}

pub fn proof(key: &[u8], nonce: &str, host_fp: &str, viewer_fp: &str, code: Option<u32>) -> String {
    hex::encode(mac(key, nonce, host_fp, viewer_fp, code).finalize().into_bytes())
}

/// Host side: does `given` prove the password (and the current TOTP code, ±30 s)?
pub fn verify(key_hex: &str, totp: &str, nonce: &str, host_fp: &str, viewer_fp: &str, given: &str) -> bool {
    let Ok(key) = hex::decode(key_hex) else { return false };
    let Ok(given) = hex::decode(given) else { return false };
    let ok = |code: Option<u32>| mac(&key, nonce, host_fp, viewer_fp, code).verify_slice(&given).is_ok(); // constant time
    if totp.is_empty() {
        return ok(None);
    }
    let Some(secret) = base32_decode(totp) else { return false };
    let now = crate::rv::now_ms() / 1000;
    [-30i64, 0, 30].iter().any(|d| ok(Some(totp_at(&secret, (now + d) as u64))))
}

// ---- trusted devices: a token the host gave out, proven like the password ----

fn trust_mac(token: &[u8], nonce: &str, host_fp: &str, viewer_fp: &str) -> Hmac<Sha256> {
    let mut mac = <Hmac<Sha256>>::new_from_slice(token).expect("hmac key");
    mac.update(b"manydesk-trust-v1");
    for part in [nonce, host_fp, viewer_fp] {
        mac.update(&(part.len() as u32).to_be_bytes());
        mac.update(part.as_bytes());
    }
    mac
}

pub fn new_trust_token() -> String {
    hex::encode(rand::random::<[u8; 32]>())
}

pub fn trust_proof(token_hex: &str, nonce: &str, host_fp: &str, viewer_fp: &str) -> String {
    hex::encode(trust_mac(&hex::decode(token_hex).unwrap_or_default(), nonce, host_fp, viewer_fp).finalize().into_bytes())
}

pub fn trust_ok(token_hex: &str, nonce: &str, host_fp: &str, viewer_fp: &str, given: &str) -> bool {
    let (Ok(token), Ok(given)) = (hex::decode(token_hex), hex::decode(given)) else { return false };
    token.len() == 32 && trust_mac(&token, nonce, host_fp, viewer_fp).verify_slice(&given).is_ok()
}

// ---- TOTP (RFC 6238: HMAC-SHA1, 30 s, 6 digits) ----

pub fn totp_at(secret: &[u8], unix: u64) -> u32 {
    let mut mac = <Hmac<Sha1>>::new_from_slice(secret).expect("hmac key");
    mac.update(&(unix / 30).to_be_bytes());
    let h = mac.finalize().into_bytes();
    let off = (h[19] & 0x0f) as usize;
    let bin = u32::from_be_bytes([h[off] & 0x7f, h[off + 1], h[off + 2], h[off + 3]]);
    bin % 1_000_000
}

pub fn new_totp_secret() -> String {
    base32_encode(&rand::random::<[u8; 20]>())
}

/// For authenticator apps (shown as a QR code).
pub fn otpauth_uri(secret: &str, id: &str) -> String {
    format!("otpauth://totp/ManyDesk:{id}?secret={secret}&issuer=ManyDesk&digits=6&period=30")
}

const B32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::new();
    let (mut buf, mut bits) = (0u32, 0);
    for &b in data {
        buf = (buf << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            out.push(B32[((buf >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(B32[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = vec![];
    let (mut buf, mut bits) = (0u32, 0);
    for c in s.chars().filter(|c| !c.is_whitespace() && *c != '=') {
        let v = B32.iter().position(|&x| x as char == c.to_ascii_uppercase())? as u32;
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            out.push((buf >> (bits - 8)) as u8);
            bits -= 8;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totp_rfc6238_vector() {
        // RFC 6238 appendix B, SHA-1, T = 59 s → 94287082 (8 digits) → 287082
        assert_eq!(totp_at(b"12345678901234567890", 59), 287082);
        assert_eq!(totp_at(b"12345678901234567890", 1111111109), 81804);
    }

    #[test]
    fn base32_round_trip() {
        let d = b"hello world, 20bytes";
        assert_eq!(base32_decode(&base32_encode(d)).unwrap(), d);
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
    }

    #[test]
    fn proof_binds_everything() {
        let salt = "00112233445566778899aabbccddeeff";
        let k = derive("correct horse", salt);
        let p = proof(&k, "n1", "hostfp", "viewfp", None);
        assert!(verify(&hex::encode(k), "", "n1", "hostfp", "viewfp", &p));
        assert!(!verify(&hex::encode(k), "", "n2", "hostfp", "viewfp", &p));
        assert!(!verify(&hex::encode(k), "", "n1", "evilfp", "viewfp", &p));
        assert!(!verify(&hex::encode(derive("wrong", salt)), "", "n1", "hostfp", "viewfp", &p));
        let secret = new_totp_secret();
        let code = totp_at(&base32_decode(&secret).unwrap(), (crate::rv::now_ms() / 1000) as u64);
        let p2 = proof(&k, "n1", "hostfp", "viewfp", Some(code));
        assert!(verify(&hex::encode(k), &secret, "n1", "hostfp", "viewfp", &p2));
        assert!(!verify(&hex::encode(k), &secret, "n1", "hostfp", "viewfp", &p));
    }

    #[test]
    fn trust_binds_session() {
        let tok = new_trust_token();
        let p = trust_proof(&tok, "n1", "hostfp", "viewfp");
        assert!(trust_ok(&tok, "n1", "hostfp", "viewfp", &p));
        assert!(!trust_ok(&tok, "n2", "hostfp", "viewfp", &p));
        assert!(!trust_ok(&new_trust_token(), "n1", "hostfp", "viewfp", &p));
    }
}
