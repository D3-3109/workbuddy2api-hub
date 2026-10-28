//! Identifier formats that must match the Python gateway exactly
//! (wb_fingerprint.py, wb_identity.py).

use md5::{Digest as _, Md5};
use rand::RngCore;
use sha2::Sha256;

fn hyphenate(h: &str) -> String {
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// `derive_id(uid, salt)`: md5 of `"salt:uid"` (uid defaults to "anonymous")
/// with the version nibble forced to 4 and the variant nibble to `a`.
pub fn derive_id(uid: &str, salt: &str) -> String {
    let uid = if uid.is_empty() { "anonymous" } else { uid };
    let digest = hex::encode(Md5::digest(format!("{salt}:{uid}").as_bytes()));
    let h = format!("{}4{}a{}", &digest[0..12], &digest[13..16], &digest[17..32]);
    hyphenate(&h)
}

pub fn machine_id(uid: &str) -> String {
    derive_id(uid, "machine")
}

pub fn session_id(uid: &str) -> String {
    derive_id(uid, "session")
}

/// RFC 9562 UUIDv7 as 16 bytes.
pub fn uuid7_bytes() -> [u8; 16] {
    let ms = (chrono::Utc::now().timestamp_millis() as u128) & ((1u128 << 48) - 1);
    let mut rng = rand::thread_rng();
    let rand_a = (rng.next_u32() & 0xfff) as u128;
    let rand_b = rng.next_u64() as u128 & ((1u128 << 62) - 1);
    let v: u128 = (ms << 80) | (0x7u128 << 76) | (rand_a << 64) | (0b10u128 << 62) | rand_b;
    v.to_be_bytes()
}

/// `str(uuid7())`: hyphenated (X-Conversation-ID).
pub fn new_conversation_id() -> String {
    hyphenate(&hex::encode(uuid7_bytes()))
}

/// `uuid7().hex`: 32 hex chars (message / request ids).
pub fn new_message_id() -> String {
    hex::encode(uuid7_bytes())
}

/// `conversation_id_for(uid, session_key)`: stable per (account, conversation).
pub fn conversation_id_for(uid: &str, session_key: &str) -> String {
    let key = session_key.trim();
    if key.is_empty() {
        return String::new();
    }
    let digest = hex::encode(Sha256::digest(format!("{uid}|{key}").as_bytes()));
    let mut h: Vec<u8> = digest.as_bytes()[..32].to_vec();
    h[12] = b'4';
    h[16] = b'a';
    hyphenate(std::str::from_utf8(&h).unwrap_or_default())
}

/// `uuid.uuid4().hex`
pub fn uuid4_hex() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// `str(uuid.uuid4())`
pub fn uuid4_str() -> String {
    uuid::Uuid::new_v4().hyphenated().to_string()
}

/// `secrets.token_hex(n)`
pub fn token_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

/// `secrets.token_urlsafe(n)`
pub fn token_urlsafe(n: usize) -> String {
    use base64::Engine as _;
    let mut buf = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

/// sha256 hex digest of a string.
pub fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_id_vectors() {
        assert_eq!(
            derive_id("u-123", "machine"),
            "98b53497-f10c-4475-a88a-9054ddd676f0"
        );
        assert_eq!(
            derive_id("u-123", "session"),
            "0ff5c5a0-043c-464b-aad4-e1557ee75825"
        );
        assert_eq!(
            derive_id("", "machine"),
            "fc1f3f60-3c8b-4dc0-a839-42ea0169ab1e"
        );
    }

    #[test]
    fn conversation_id_vector() {
        assert_eq!(
            conversation_id_for("u-123", "pfx-0123456789abcdef"),
            "c26f60d7-a3c6-4ac1-a98a-d43ea8b5b6a1"
        );
        assert_eq!(conversation_id_for("u", "  "), "");
    }

    #[test]
    fn uuid7_shape() {
        let id = new_message_id();
        assert_eq!(id.len(), 32);
        assert_eq!(&id[12..13], "7");
        let c = new_conversation_id();
        assert_eq!(c.len(), 36);
    }
}
