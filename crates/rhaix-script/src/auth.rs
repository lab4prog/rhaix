//! Паролі: `hash_password` і `verify_password`.
//!
//! Відкладено сюди з M0 навмисно. Правильна відповідь на «як зберігати пароль»
//! — Argon2id із випадковою сіллю, а не SHA-256: швидкий геш підбирається на
//! GPU мільярдами за секунду, повільний із пам'яттю — ні. Видавати `sha256()`
//! за хешування паролів було б обманом, тому його там і не було.
//!
//! Параметри — стандартні OWASP для Argon2id (19 МіБ, 2 проходи). Сіль
//! генерується на кожен пароль і зберігається в самому рядку-хеші (формат PHC),
//! тож окремої колонки для солі не треба.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rhai::{Dynamic, Engine};

use crate::crypto::random_bytes;

/// Захешувати пароль. Повертає рядок PHC — його й кладуть у колонку.
///
/// Порожній пароль хешувати немає сенсу — це майже завжди помилка у формі,
/// тому повертаємо `()`, щоб виклик було видно як невдалий, а не записати в
/// базу хеш від порожнечі.
pub fn hash_password(password: &str) -> Dynamic {
    if password.is_empty() {
        return Dynamic::UNIT;
    }
    // 16 байтів солі з системного джерела.
    let salt = SaltString::encode_b64(&random_bytes(16)).expect("16 байтів завжди кодуються");
    match Argon2::default().hash_password(password.as_bytes(), &salt) {
        Ok(hash) => Dynamic::from(hash.to_string()),
        Err(err) => {
            // Єдина реальна причина — брак пам'яті під час хешування; краще
            // сказати, ніж мовчки віддати `()`.
            tracing::error!("hash_password: {err}");
            Dynamic::UNIT
        }
    }
}

/// Перевірити пароль проти збереженого хешу. Порівняння сталого часу — усередині
/// `argon2`. Зіпсований або чужого формату хеш дає `false`, а не помилку: для
/// сторінки входу це просто «пароль не підійшов».
pub fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

pub fn register_auth(engine: &mut Engine) {
    engine
        .register_fn("hash_password", hash_password)
        .register_fn("verify_password", verify_password);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_round_trips() {
        let hash = hash_password("правильний-кінь")
            .into_string()
            .expect("є хеш");
        // Формат PHC із назвою алгоритму — щоб було видно, що це саме Argon2.
        assert!(hash.starts_with("$argon2id$"), "{hash}");
        assert!(verify_password("правильний-кінь", &hash));
        assert!(!verify_password("неправильний", &hash));
    }

    #[test]
    fn the_same_password_hashes_differently() {
        // Різна сіль на кожен виклик — два однакові паролі дають різні хеші,
        // тому за базою не видно, у кого паролі збігаються.
        let a = hash_password("однаковий").into_string().unwrap();
        let b = hash_password("однаковий").into_string().unwrap();
        assert_ne!(a, b);
        assert!(verify_password("однаковий", &a));
        assert!(verify_password("однаковий", &b));
    }

    #[test]
    fn an_empty_password_is_rejected() {
        assert!(hash_password("").is_unit());
    }

    #[test]
    fn a_garbage_hash_is_false_not_an_error() {
        assert!(!verify_password("будь-що", "не-PHC-рядок"));
        assert!(!verify_password("будь-що", ""));
    }
}
