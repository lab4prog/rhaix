//! `session` і `csrf` — підписані cookie.
//!
//! Сервера з сесіями в пам'яті тут немає навмисно: застосунок rhaix має
//! лишатися одним бінарником, який можна запустити у двох копіях за балансиром.
//! Тому весь стан сесії їде до клієнта й назад у cookie, підписаному
//! HMAC-SHA256. Клієнт бачить вміст (base64 — не шифр), але не може його
//! змінити.
//!
//! Наслідок, про який треба знати: **у сесію не кладуть багато**. Cookie
//! обмежене ~4 КБ; за 3 КБ ми попереджаємо в лог.

use std::sync::{Arc, Mutex};

use rhai::{Dynamic, Engine, Map};

use crate::crypto::{base64url_decode, base64url_encode, random_token, sign, verify};
use crate::datetime::now_secs;
use crate::json;

/// Ім'я поля з CSRF-токеном — і у формі, і всередині сесії.
pub const CSRF_FIELD: &str = "_csrf";
/// Заголовок-альтернатива полю форми: для `hx-headers` і JSON-запитів.
pub const CSRF_HEADER: &str = "x-csrf-token";

const MAX_COOKIE: usize = 3072;

/// Налаштування cookie сесії.
#[derive(Debug, Clone)]
pub struct SessionOptions {
    pub cookie: String,
    /// Скільки живе сесія, у секундах.
    pub max_age: i64,
    /// `Secure` — лише для HTTPS. У dev вимкнено, інакше cookie не поїде на localhost.
    pub secure: bool,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            cookie: "rhaix_session".to_owned(),
            max_age: 60 * 60 * 24 * 30,
            secure: false,
        }
    }
}

#[derive(Debug, Default)]
struct Inner {
    data: Map,
    /// Чи змінювали сесію в цьому запиті — тоді й тільки тоді ставимо cookie.
    dirty: bool,
}

/// `session` у скрипті.
#[derive(Debug, Default, Clone)]
pub struct Session(Arc<Mutex<Inner>>);

impl Session {
    /// Порожня сесія — коли cookie немає або підпис не зійшовся.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Відновити сесію з cookie. Зіпсований або прострочений cookie мовчки
    /// дає порожню сесію: для користувача це просто «мене розлогінило».
    pub fn restore(raw: Option<&str>, secret: &[u8]) -> Self {
        let Some(raw) = raw else {
            return Self::empty();
        };
        let Some((payload, signature)) = raw.rsplit_once('.') else {
            return Self::empty();
        };
        if !verify(secret, payload.as_bytes(), signature) {
            return Self::empty();
        }
        let Some(bytes) = base64url_decode(payload) else {
            return Self::empty();
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return Self::empty();
        };
        let Some(value) = json::parse(&text) else {
            return Self::empty();
        };
        let Some(envelope) = value.try_cast::<Map>() else {
            return Self::empty();
        };

        let expires = envelope
            .get("e")
            .and_then(|v| v.as_int().ok())
            .unwrap_or_default();
        if expires <= now_secs() {
            return Self::empty();
        }
        let data = envelope
            .get("d")
            .and_then(|v| v.clone().try_cast::<Map>())
            .unwrap_or_default();

        Self(Arc::new(Mutex::new(Inner { data, dirty: false })))
    }

    fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        let mut guard = self.0.lock().expect("сесія не отруєна");
        f(&mut guard)
    }

    pub fn get(&self, key: &str) -> Dynamic {
        self.with(|inner| inner.data.get(key).cloned().unwrap_or(Dynamic::UNIT))
    }

    pub fn set(&self, key: &str, value: Dynamic) {
        self.with(|inner| {
            inner.data.insert(key.into(), value);
            inner.dirty = true;
        });
    }

    pub fn remove(&self, key: &str) {
        self.with(|inner| {
            if inner.data.remove(key).is_some() {
                inner.dirty = true;
            }
        });
    }

    pub fn clear(&self) {
        self.with(|inner| {
            inner.data.clear();
            inner.dirty = true;
        });
    }

    pub fn is_dirty(&self) -> bool {
        self.with(|inner| inner.dirty)
    }

    pub fn is_empty(&self) -> bool {
        self.with(|inner| inner.data.is_empty())
    }

    /// Готовий рядок `Set-Cookie`, або `None`, якщо сесію не чіпали.
    pub fn cookie(&self, secret: &[u8], options: &SessionOptions) -> Option<String> {
        if !self.is_dirty() {
            return None;
        }
        let flags = format!(
            "Path=/; HttpOnly; SameSite=Lax{}",
            if options.secure { "; Secure" } else { "" }
        );
        if self.is_empty() {
            // `session.clear()` має гасити cookie, а не лишати порожній конверт.
            return Some(format!("{}=; Max-Age=0; {flags}", options.cookie));
        }

        let data = self.with(|inner| inner.data.clone());
        let mut envelope = Map::new();
        envelope.insert("e".into(), Dynamic::from(now_secs() + options.max_age));
        envelope.insert("d".into(), Dynamic::from_map(data));
        let payload = base64url_encode(
            json::from_dynamic(&Dynamic::from_map(envelope))
                .to_string()
                .as_bytes(),
        );
        let signature = sign(secret, payload.as_bytes());
        let value = format!("{payload}.{signature}");

        if value.len() > MAX_COOKIE {
            tracing::warn!(
                "сесія завелика ({} б): браузери обмежують cookie ~4 КБ. \
                 Тримайте в сесії ідентифікатори, а дані — в базі",
                value.len()
            );
        }
        Some(format!(
            "{}={value}; Max-Age={}; {flags}",
            options.cookie, options.max_age
        ))
    }
}

/// `csrf` у скрипті й у розмітці.
///
/// Токен народжується **ліниво**: сторінка без форм не отримує ні токена, ні
/// cookie. Інакше кожен анонімний відвідувач ніс би сесію ні за що.
#[derive(Debug, Clone)]
pub struct Csrf {
    session: Session,
    enabled: bool,
}

impl Csrf {
    pub fn new(session: Session, enabled: bool) -> Self {
        Self { session, enabled }
    }

    /// Токен цієї сесії; створюється при першому звертанні.
    pub fn token(&self) -> String {
        if !self.enabled {
            return String::new();
        }
        if let Ok(existing) = self.session.get(CSRF_FIELD).into_string() {
            if !existing.is_empty() {
                return existing;
            }
        }
        let token = random_token(24);
        self.session.set(CSRF_FIELD, Dynamic::from(token.clone()));
        token
    }

    /// Перевірити те, що прийшло з формою. Токена не створює: інакше перевірка
    /// сама б собі видавала перепустку.
    pub fn verify(&self, supplied: Option<&str>) -> bool {
        if !self.enabled {
            return true;
        }
        let Ok(expected) = self.session.get(CSRF_FIELD).into_string() else {
            return false;
        };
        let Some(supplied) = supplied else {
            return false;
        };
        !expected.is_empty() && constant_time_eq(expected.as_bytes(), supplied.as_bytes())
    }
}

/// Порівняння, час якого не залежить від того, де саме розійшлись рядки.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right) {
        diff |= a ^ b;
    }
    diff == 0
}

/// Секрет для підпису. Порядок пошуку описано в `Config::secret`.
#[derive(Clone)]
pub struct Secret(Arc<Vec<u8>>);

impl Secret {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Arc::new(bytes))
    }

    /// Випадковий секрет на час життя процесу.
    pub fn ephemeral() -> Self {
        Self::new(crate::crypto::random_bytes(32))
    }

    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Секрет не потрапляє в логи навіть випадково.
        f.write_str("Secret(<приховано>)")
    }
}

pub fn register_session(engine: &mut Engine) {
    engine
        .register_type_with_name::<Session>("Session")
        .register_fn("get", |session: &mut Session, key: &str| session.get(key))
        .register_fn("set", |session: &mut Session, key: &str, value: Dynamic| {
            session.set(key, value)
        })
        .register_fn("has", |session: &mut Session, key: &str| {
            !session.get(key).is_unit()
        })
        .register_fn("remove", |session: &mut Session, key: &str| {
            session.remove(key)
        })
        .register_fn("clear", |session: &mut Session| session.clear())
        .register_fn("all", |session: &mut Session| {
            session.with(|inner| Dynamic::from_map(inner.data.clone()))
        })
        // `session.user` — те саме, що `session.get("user")`, але коротше:
        // це найчастіше поле в будь-якому застосунку.
        .register_get("user", |session: &mut Session| session.get("user"));

    engine
        .register_type_with_name::<Csrf>("Csrf")
        .register_get("token", |csrf: &mut Csrf| csrf.token())
        .register_fn("token", |csrf: &mut Csrf| csrf.token());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret() -> Secret {
        Secret::new(b"test secret".to_vec())
    }

    /// Витягти значення cookie з рядка `Set-Cookie`.
    fn value_of(header: &str) -> String {
        header
            .split_once('=')
            .and_then(|(_, rest)| rest.split(';').next())
            .unwrap_or_default()
            .to_owned()
    }

    #[test]
    fn untouched_session_sets_no_cookie() {
        let session = Session::empty();
        let _ = session.get("user");
        assert!(session
            .cookie(secret().bytes(), &SessionOptions::default())
            .is_none());
    }

    #[test]
    fn session_survives_a_round_trip() {
        let session = Session::empty();
        session.set("user", Dynamic::from("оля".to_owned()));
        session.set("admin", Dynamic::from(true));
        let header = session
            .cookie(secret().bytes(), &SessionOptions::default())
            .expect("сесію змінили — має бути cookie");
        assert!(header.contains("HttpOnly"), "{header}");
        assert!(header.contains("SameSite=Lax"), "{header}");

        let restored = Session::restore(Some(&value_of(&header)), secret().bytes());
        assert_eq!(restored.get("user").into_string().unwrap(), "оля");
        assert!(restored.get("admin").as_bool().unwrap());
        assert!(!restored.is_dirty(), "відновлення — не зміна");
    }

    #[test]
    fn tampering_drops_the_session() {
        let session = Session::empty();
        session.set("admin", Dynamic::from(false));
        let header = session
            .cookie(secret().bytes(), &SessionOptions::default())
            .expect("cookie");
        let raw = value_of(&header);

        // Підміна корисного навантаження без підпису — сесія просто зникає.
        let (_, signature) = raw.rsplit_once('.').expect("є підпис");
        let forged = base64url_encode(br#"{"e":99999999999,"d":{"admin":true}}"#);
        let restored = Session::restore(Some(&format!("{forged}.{signature}")), secret().bytes());
        assert!(restored.is_empty(), "підроблена сесія має бути порожньою");

        // Той самий cookie, але інший секрет — теж повз.
        let other = Session::restore(Some(&raw), b"another secret");
        assert!(other.is_empty());
    }

    #[test]
    fn expired_session_is_ignored() {
        let mut envelope = Map::new();
        envelope.insert("e".into(), Dynamic::from(now_secs() - 1));
        envelope.insert("d".into(), Dynamic::from_map(Map::new()));
        let payload = base64url_encode(
            json::from_dynamic(&Dynamic::from_map(envelope))
                .to_string()
                .as_bytes(),
        );
        let signature = sign(secret().bytes(), payload.as_bytes());
        let restored = Session::restore(Some(&format!("{payload}.{signature}")), secret().bytes());
        assert!(restored.is_empty());
    }

    #[test]
    fn clear_expires_the_cookie() {
        let session = Session::empty();
        session.set("user", Dynamic::from("оля".to_owned()));
        session.clear();
        let header = session
            .cookie(secret().bytes(), &SessionOptions::default())
            .expect("cookie");
        assert!(header.contains("Max-Age=0"), "{header}");
    }

    #[test]
    fn csrf_token_is_lazy_and_stable() {
        let session = Session::empty();
        let csrf = Csrf::new(session.clone(), true);
        assert!(!session.is_dirty(), "без форми — без токена й без cookie");

        let token = csrf.token();
        assert_eq!(token, csrf.token(), "у межах запиту токен один");
        assert!(session.is_dirty());
        assert!(csrf.verify(Some(&token)));
        assert!(!csrf.verify(Some("чужий")));
        assert!(!csrf.verify(None));
    }

    #[test]
    fn csrf_verify_never_mints_a_token() {
        let session = Session::empty();
        let csrf = Csrf::new(session.clone(), true);
        assert!(!csrf.verify(Some("будь-що")));
        assert!(session.is_empty(), "перевірка не має створювати токен");
    }

    #[test]
    fn disabled_csrf_lets_everything_through() {
        let csrf = Csrf::new(Session::empty(), false);
        assert!(csrf.verify(None));
        assert!(csrf.token().is_empty());
    }
}
