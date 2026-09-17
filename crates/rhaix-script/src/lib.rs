//! Налаштування Rhai-рушія і правила перетворення значень, спільні для всього rhaix.
//!
//! Тут живуть рішення зі специфікації, які легко «розповзаються» по коду, якщо їх
//! не зафіксувати в одному місці: що виводиться замість `()`, що вважається
//! хибним в `@if`, які ліміти стоять на скрипті користувача.

mod auth;
mod crypto;
mod data;
mod datetime;
mod http;
mod json;
mod session;
mod stdlib;
mod text;
mod web;

pub use auth::{hash_password, verify_password};
pub use crypto::{random_token, uuid_v4};
pub use data::register_db;
pub use datetime::{now_secs, parse_tz_offset, register_datetime, set_tz_offset};
pub use http::{register_http, Http};
pub use json::{parse as json_parse, to_dynamic as json_to_dynamic};
pub use session::{
    register_session, Csrf, Secret, Session, SessionOptions, CSRF_FIELD, CSRF_HEADER,
};
pub use stdlib::{register_core, Html, SlotSet};
pub use text::register_text;
pub use web::{
    parse_cookies, parse_urlencoded, register_web, triggers_header, Hx, Log, Request, RequestData,
    Response, ResponseData, State,
};

use std::cell::Cell;
use std::time::{Duration, Instant};

use rhai::{Dynamic, Engine, EvalAltResult, OptimizationLevel, AST};

/// Ліміти за замовчуванням. Сенс у тому, щоб помилка користувача (нескінченний
/// цикл, будування рядка на гігабайт) впиралась у зрозумілу межу, а не клала сервер.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_operations: u64,
    pub max_call_levels: usize,
    pub max_string_size: usize,
    pub max_expr_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_operations: 10_000_000,
            max_call_levels: 64,
            max_string_size: 16 * 1024 * 1024,
            max_expr_depth: 64,
        }
    }
}

/// Рушій для frontmatter і виразів шаблону.
pub fn engine(limits: Limits) -> Engine {
    let mut engine = Engine::new();
    engine.set_max_operations(limits.max_operations);
    engine.set_max_call_levels(limits.max_call_levels);
    engine.set_max_string_size(limits.max_string_size);
    engine.set_max_expr_depths(limits.max_expr_depth, limits.max_expr_depth);
    engine.set_optimization_level(OptimizationLevel::Full);

    // Ліміт часу: скрипт користувача не має тримати потік нескінченно.
    // Перевіряємо не щокроку, а раз на кілька тисяч операцій — цього достатньо,
    // щоб зловити цикл, і не видно на звичайному рендері.
    engine.on_progress(|operations| {
        if operations % 4096 != 0 {
            return None;
        }
        deadline_exceeded().then(|| Dynamic::from("час виконання скрипта вичерпано"))
    });

    register_core(&mut engine);
    register_web(&mut engine);
    register_db(&mut engine);
    register_session(&mut engine);
    register_http(&mut engine);
    register_datetime(&mut engine);
    register_text(&mut engine);
    auth::register_auth(&mut engine);
    engine
}

thread_local! {
    static DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Обмежувач часу на один запит.
///
/// Рендер іде в `spawn_blocking`, тобто один запит — один потік, тому дедлайн
/// живе в thread-local і знімається сам, коли `Deadline` виходить з області
/// видимості.
#[must_use = "дедлайн діє, поки живе цей об'єкт"]
pub struct Deadline;

impl Deadline {
    pub fn new(budget: Duration) -> Self {
        DEADLINE.with(|cell| cell.set(Some(Instant::now() + budget)));
        Self
    }
}

/// Відсунути дедлайн на час, проведений у очікуванні вводу-виводу.
///
/// Скрипт, який чекає на відповідь чужого API, не «крутиться»: рахувати цей
/// час проти бюджету сторінки неправильно — вона впала б уже після того, як
/// дані приїхали.
pub fn extend_deadline(by: Duration) {
    DEADLINE.with(|cell| {
        if let Some(at) = cell.get() {
            cell.set(Some(at + by));
        }
    });
}

impl Drop for Deadline {
    fn drop(&mut self) {
        DEADLINE.with(|cell| cell.set(None));
    }
}

fn deadline_exceeded() -> bool {
    DEADLINE.with(|cell| match cell.get() {
        Some(at) => Instant::now() >= at,
        None => false,
    })
}

/// Скомпілювати вміст `{{ ... }}`.
///
/// Саме `compile_expression`, а не `compile`: за специфікацією (SYNTAX 2.1) в
/// інтерполяції дозволений вираз, а не інструкції — і помилка про це має
/// з'являтися при компіляції шаблону, а не під час запиту.
pub fn compile_expression(engine: &Engine, expr: &str) -> Result<AST, Box<EvalAltResult>> {
    engine.compile_expression(expr).map_err(|err| err.into())
}

/// Значення → текст для виводу в HTML.
///
/// `()` і `false` дають порожній рядок: інакше кожне порожнє поле з БД
/// друкувало б `()`, а `{{ flag }}` — слово `false` (SYNTAX 2.1).
pub fn display(value: &Dynamic) -> String {
    if value.is_unit() {
        return String::new();
    }
    if let Ok(flag) = value.as_bool() {
        return if flag {
            "true".to_owned()
        } else {
            String::new()
        };
    }
    value.to_string()
}

/// Те саме, що [`display`], але без проміжного `String`.
///
/// Рендерер пише значення прямо в буфер відповіді, тому алокація на кожне поле
/// таблиці — це чистий податок. Для типів, які реально приходять з БД
/// (ціле, рядок, булеве), запис іде без жодної алокації.
pub fn write_display(out: &mut String, value: &Dynamic) {
    use std::fmt::Write as _;

    if value.is_unit() {
        return;
    }
    if let Ok(flag) = value.as_bool() {
        if flag {
            out.push_str("true");
        }
        return;
    }
    if let Ok(n) = value.as_int() {
        let _ = write!(out, "{n}");
        return;
    }
    if let Some(text) = value.read_lock::<rhai::ImmutableString>() {
        out.push_str(&text);
        return;
    }
    if let Some(html) = value.read_lock::<Html>() {
        out.push_str(&html.0);
        return;
    }
    if let Ok(f) = value.as_float() {
        let _ = write!(out, "{f}");
        return;
    }
    out.push_str(&value.to_string());
}

/// Істинність для `@if` (SYNTAX 4.1).
///
/// Свідомо м'якша за Rhai й однакова з Jinja/Twig/PHP: `0`, `""`, порожня
/// колекція — хибні. Це очікувана поведінка для цільової аудиторії, але вона ж
/// є відомим підводним каменем (`@if={price}` з ціною 0), тому описана в доках.
pub fn truthy(value: &Dynamic) -> bool {
    if value.is_unit() {
        return false;
    }
    if let Ok(flag) = value.as_bool() {
        return flag;
    }
    if let Ok(n) = value.as_int() {
        return n != 0;
    }
    if let Ok(f) = value.as_float() {
        return f != 0.0;
    }
    if let Some(s) = value.read_lock::<rhai::ImmutableString>() {
        return !s.is_empty();
    }
    if let Some(arr) = value.read_lock::<rhai::Array>() {
        return !arr.is_empty();
    }
    if let Some(map) = value.read_lock::<rhai::Map>() {
        return !map.is_empty();
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhai::{Array, Map};

    #[test]
    fn unit_and_false_render_as_empty_string() {
        assert_eq!(display(&Dynamic::UNIT), "");
        assert_eq!(display(&Dynamic::from(false)), "");
        assert_eq!(display(&Dynamic::from(true)), "true");
        assert_eq!(display(&Dynamic::from(42_i64)), "42");
        assert_eq!(display(&Dynamic::from("текст")), "текст");
    }

    #[test]
    fn write_display_matches_display() {
        for value in [
            Dynamic::UNIT,
            Dynamic::from(false),
            Dynamic::from(true),
            Dynamic::from(42_i64),
            Dynamic::from("текст"),
            Dynamic::from(1.5_f64),
        ] {
            let mut buf = String::new();
            write_display(&mut buf, &value);
            assert_eq!(buf, display(&value), "{value:?}");
        }
    }

    #[test]
    fn truthiness_matches_the_spec() {
        assert!(!truthy(&Dynamic::UNIT));
        assert!(!truthy(&Dynamic::from(false)));
        assert!(!truthy(&Dynamic::from(0_i64)));
        assert!(!truthy(&Dynamic::from("")));
        assert!(!truthy(&Dynamic::from(Array::new())));
        assert!(!truthy(&Dynamic::from(Map::new())));

        assert!(truthy(&Dynamic::from(1_i64)));
        assert!(truthy(&Dynamic::from("x")));
        assert!(truthy(&Dynamic::from(vec![Dynamic::from(1_i64)])));
    }

    #[test]
    fn statements_are_rejected_in_interpolation() {
        let engine = engine(Limits::default());
        assert!(compile_expression(&engine, "user.name").is_ok());
        assert!(compile_expression(&engine, "price * qty").is_ok());
        assert!(compile_expression(&engine, "let x = 1; x").is_err());
    }

    #[test]
    fn deadline_stops_a_long_script() {
        let engine = engine(Limits::default());
        let _guard = Deadline::new(Duration::from_millis(30));
        let err = engine
            .eval::<i64>("let i = 0; while true { i += 1 } i")
            .unwrap_err();
        assert!(
            matches!(*err, rhai::EvalAltResult::ErrorTerminated(..)),
            "{err}"
        );
    }

    #[test]
    fn limits_stop_runaway_scripts() {
        let engine = engine(Limits {
            max_operations: 10_000,
            ..Limits::default()
        });
        let err = engine
            .eval::<i64>("let i = 0; while true { i += 1 } i")
            .unwrap_err();
        assert!(
            matches!(*err, rhai::EvalAltResult::ErrorTooManyOperations(_)),
            "{err}"
        );
    }
}
