// This install's long-term identity: a secp256k1 Schnorr key kept in settings.json.
//
// A 9-digit ID only says where a computer listens, and anyone who knows it can answer
// in its name. So each side signs what it sends with this key (inside the encrypted
// envelopes, where relays do not see it), and the other side remembers the key the
// first time it connects (trust on first use, as SSH does): a later answer signed with
// another key, or not signed at all, is not that computer.
//
// Signed messages (ctx, then the parts, each length-prefixed, hashed with SHA-256):
//   "manydesk-req-v1"   host ID, reply topic, session key, from ID, mode, sts  (viewer)
//   "manydesk-offer-v1" host ID, viewer reply topic, host reply topic, cert fp  (host)
//   "manydesk-hi-v1"    ID, MACs, own IPs, ts                                   (LAN)
use std::sync::OnceLock;

use k256::schnorr::{Signature, SigningKey, VerifyingKey};
use rand::RngCore;
use sha2::{Digest, Sha256};

static KEY: OnceLock<SigningKey> = OnceLock::new();

/// How far a signed timestamp may be from our clock (ms).
pub const MAX_SKEW_MS: i64 = 10 * 60 * 1000;

/// A new secret key (hex), for a settings file that has none.
pub fn generate() -> String {
    hex::encode(SigningKey::random(&mut rand::thread_rng()).to_bytes())
}

/// Use this secret (hex) from now on. Only the first call counts; a key that does not
/// parse leaves an ephemeral one (this run then signs with a key nobody has pinned).
pub fn init(secret_hex: &str) {
    let key = hex::decode(secret_hex).ok().and_then(|b| SigningKey::from_bytes(&b).ok());
    let _ = KEY.set(key.unwrap_or_else(|| SigningKey::random(&mut rand::thread_rng())));
}

fn key() -> &'static SigningKey {
    KEY.get_or_init(|| SigningKey::random(&mut rand::thread_rng()))
}

/// Our public key: 64 hex digits (x-only, BIP-340).
pub fn public() -> String {
    hex::encode(key().verifying_key().to_bytes())
}

fn digest(ctx: &str, parts: &[&str]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(ctx.as_bytes());
    h.update([0u8]);
    for p in parts {
        h.update((p.len() as u32).to_be_bytes());
        h.update(p.as_bytes());
    }
    h.finalize().into()
}

/// Signature (128 hex digits) over the context and parts.
pub fn sign(ctx: &str, parts: &[&str]) -> String {
    let mut aux = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut aux);
    key().sign_raw(&digest(ctx, parts), &aux).map(|s| hex::encode(s.to_bytes())).unwrap_or_default()
}

/// Whether `sig` is `pubkey`'s signature over the context and parts.
pub fn verify(pubkey: &str, ctx: &str, parts: &[&str], sig: &str) -> bool {
    let Some(pk) = hex::decode(pubkey).ok().and_then(|b| VerifyingKey::from_bytes(&b).ok()) else { return false };
    // (k256 panics on a slice that is not 64 bytes)
    let Some(sig) = hex::decode(sig).ok().filter(|b| b.len() == 64).and_then(|b| Signature::try_from(b.as_slice()).ok()) else { return false };
    pk.verify_raw(&digest(ctx, parts), &sig).is_ok()
}

/// A well-formed public key (64 hex digits).
pub fn is_key(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A signed timestamp close enough to now.
pub fn fresh(ts_ms: i64) -> bool {
    (crate::rv::now_ms() - ts_ms).abs() <= MAX_SKEW_MS
}

/// What people compare: "AB12 CD34 EF56 7890" from the key's hash.
pub fn code(pubkey: &str) -> String {
    let h = Sha256::digest(format!("manydesk-identity-v1:{pubkey}").as_bytes());
    let s = hex::encode_upper(&h[..8]);
    s.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap_or("")).collect::<Vec<_>>().join(" ")
}

/// What a peer's signature says about it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Check {
    /// signed by the key we remember, or the first one (now remembered)
    Verified,
    /// no signature (an older ManyDesk), and no key remembered for it
    Unverified,
    /// a different key than remembered, a bad signature, or unsigned where a key is known
    Mismatch,
}

/// Compares what came (`got`: the key that verifiably signed, or None for unsigned,
/// Err for a bad signature) with what we remember (`pinned`, empty = nothing yet).
pub fn check(pinned: &str, got: Result<Option<&str>, ()>) -> Check {
    match got {
        Err(()) => Check::Mismatch,
        Ok(None) if pinned.is_empty() => Check::Unverified,
        Ok(None) => Check::Mismatch,
        Ok(Some(k)) if pinned.is_empty() || pinned.eq_ignore_ascii_case(k) => Check::Verified,
        Ok(Some(_)) => Check::Mismatch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify() {
        let pk = public();
        assert!(is_key(&pk));
        let sig = sign("ctx", &["a", "b"]);
        assert!(verify(&pk, "ctx", &["a", "b"], &sig));
        // other context, other parts, moved boundary, other key
        assert!(!verify(&pk, "ctx2", &["a", "b"], &sig));
        assert!(!verify(&pk, "ctx", &["a", "c"], &sig));
        assert!(!verify(&pk, "ctx", &["ab", ""], &sig));
        let other = hex::encode(SigningKey::random(&mut rand::thread_rng()).verifying_key().to_bytes());
        assert!(!verify(&other, "ctx", &["a", "b"], &sig));
        assert!(!verify(&pk, "ctx", &["a", "b"], "00"));
        assert!(!verify("zz", "ctx", &["a", "b"], &sig));
    }

    #[test]
    fn pin_checks() {
        let k = "a".repeat(64);
        let j = "b".repeat(64);
        assert_eq!(check("", Ok(Some(&k))), Check::Verified);
        assert_eq!(check(&k, Ok(Some(&k))), Check::Verified);
        assert_eq!(check(&k, Ok(Some(&j))), Check::Mismatch);
        assert_eq!(check("", Ok(None)), Check::Unverified);
        assert_eq!(check(&k, Ok(None)), Check::Mismatch);
        assert_eq!(check("", Err(())), Check::Mismatch);
    }

    #[test]
    fn code_is_short_and_stable() {
        let c = code(&"a".repeat(64));
        assert_eq!(c.len(), 19);
        assert_eq!(c, code(&"a".repeat(64)));
        assert_ne!(c, code(&"b".repeat(64)));
    }
}
