//! Дрібна криптографія: підпис cookie, випадкові токени, base64url.
//!
//! Тут навмисно мало: HMAC-SHA256 для підпису сесії, `getrandom` для токенів
//! і власний base64url без залежності — формат простий, а зайвий крейт нічого
//! не додає.
//!
//! Чого тут **немає**: хешування паролів. Правильний `hash_password` — це
//! Argon2 з підбором параметрів, і робити вигляд, що SHA-256 його замінює,
//! нечесно. Це частина M12 (auth), разом із рештою батарейок.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Підписати повідомлення ключем. Повертає підпис у base64url.
pub fn sign(secret: &[u8], message: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC приймає ключ будь-якої довжини");
    mac.update(message);
    base64url_encode(&mac.finalize().into_bytes())
}

/// Перевірити підпис. Порівняння сталого часу — усередині `hmac`.
pub fn verify(secret: &[u8], message: &[u8], signature: &str) -> bool {
    let Some(expected) = base64url_decode(signature) else {
        return false;
    };
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC приймає ключ будь-якої довжини");
    mac.update(message);
    mac.verify_slice(&expected).is_ok()
}

/// SHA-256 у hex — для `sha256()` у скрипті.
pub fn sha256_hex(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// `n` випадкових байтів із системного джерела.
pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut buffer = vec![0u8; n];
    // Якщо ОС не дала випадковості, продовжувати не можна: підписи й токени
    // стануть передбачуваними. Краще впасти голосно.
    getrandom::fill(&mut buffer).expect("система має джерело випадкових чисел");
    buffer
}

/// Випадковий токен у base64url — сесійний ключ, CSRF, `random_id()`.
pub fn random_token(bytes: usize) -> String {
    base64url_encode(&random_bytes(bytes))
}

/// UUID v4 у канонічному записі.
pub fn uuid_v4() -> String {
    let mut bytes = random_bytes(16);
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // версія 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // варіант RFC 4122
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

// ------------------------------------------------------------- base64url

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// base64url без вирівнювання: рівно те, що можна класти в cookie й URL.
pub fn base64url_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18) as usize & 63] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(triple >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[triple as usize & 63] as char);
        }
    }
    out
}

pub fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for ch in text.bytes() {
        let value = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            b'=' => continue,
            _ => return None,
        } as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_round_trip() {
        for size in 0..40 {
            let bytes = random_bytes(size);
            let text = base64url_encode(&bytes);
            assert!(
                !text.contains('+') && !text.contains('/') && !text.contains('='),
                "{text}"
            );
            assert_eq!(base64url_decode(&text).expect("розбір"), bytes);
        }
    }

    #[test]
    fn signature_detects_any_change() {
        let secret = "ключ".as_bytes();
        let signature = sign(secret, b"payload");
        assert!(verify(secret, b"payload", &signature));
        assert!(!verify(secret, b"payloaD", &signature));
        assert!(!verify("інший".as_bytes(), b"payload", &signature));
        assert!(!verify(secret, b"payload", "не base64!!"));
    }

    #[test]
    fn uuid_has_version_and_variant() {
        let id = uuid_v4();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"), "{id}");
        assert_ne!(id, uuid_v4());
    }

    #[test]
    fn sha256_matches_the_known_vector() {
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
