//! Базова бібліотека, доступна в кожному `.rhx`.
//!
//! Тут лише те, без чого не працюють правила самої спеки: тип `Html` і `raw()`
//! (єдиний вихід з екранування), `json()` (єдиний спосіб віддати дані в
//! `<script>`) і `url()` (єдиний безпечний спосіб зібрати посилання зі станом).
//! Решта stdlib — дати, рядки, числа — приїде в M5 окремим модулем.

use rhai::{Dynamic, Engine, EvalAltResult, ImmutableString, Map};

/// Готовий HTML, який не екранується повторно.
///
/// Усе, що виводиться в шаблон, екранується — крім значень цього типу. Тому
/// вийти з екранування можна тільки явно: `raw(...)`, `json(...)`, або з
/// функції, яка свідомо повертає `Html` (наприклад, `markdown()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Html(pub String);

impl Html {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Html {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Які слоти передав батько — видно в компоненті як `slots`.
#[derive(Debug, Default, Clone)]
pub struct SlotSet(std::sync::Arc<Vec<String>>);

impl SlotSet {
    pub fn new(names: Vec<String>) -> Self {
        Self(std::sync::Arc::new(names))
    }
}

/// Зареєструвати базові функції в рушії.
pub fn register_core(engine: &mut Engine) {
    // `trim()` у Rhai змінює рядок на місці й повертає `()`. Через це
    // `let title = req.form("title").trim();` тихо давав порожнє значення —
    // знайдено на демо-формі в M2. Перекриваємо своєю версією: вона так само
    // підрізає рядок на місці, але ще й повертає результат, тому обидва
    // способи запису працюють однаково.
    engine.register_fn("trim", |text: &mut ImmutableString| -> ImmutableString {
        let trimmed: ImmutableString = text.trim().into();
        *text = trimmed.clone();
        trimmed
    });

    engine
        .register_type_with_name::<SlotSet>("SlotSet")
        .register_fn("has", |slots: &mut SlotSet, name: &str| {
            slots.0.iter().any(|found| found == name)
        })
        .register_fn("is_empty", |slots: &mut SlotSet| slots.0.is_empty());

    // У SQLite немає булевого типу: `done` приїжджає як 0 або 1, і `!todo.done`
    // у Rhai падало б із «невідома функція `! (i64)`». Заперечення й `bool()`
    // працюють за тим самим правилом істинності, що й `@if` у шаблоні.
    engine.register_fn("!", |number: i64| number == 0);
    engine.register_fn("!", |number: f64| number == 0.0);
    engine.register_fn("bool", |value: Dynamic| super::truthy(&value));

    engine.register_type_with_name::<Html>("Html");
    engine.register_fn("to_string", |html: &mut Html| html.0.clone());
    engine.register_fn("raw", raw);
    engine.register_fn("json", json);
    engine.register_fn("url", url_with_params);
    engine.register_fn("url", url_plain);
    engine.register_fn("percent", percent);
}

/// `raw(x)` — віддати значення як готовий HTML, без екранування.
fn raw(value: Dynamic) -> Html {
    if let Some(html) = value.read_lock::<Html>() {
        return html.clone();
    }
    Html(super::display(&value))
}

/// `json(x)` — значення для `<script>`: JSON плюс екранування символів, які
/// можуть закрити тег або зламати JS-рядок.
///
/// HTML-екранування тут не працює (усередині JS-рядка `&lt;` — це просто текст),
/// тому для контексту `<script>` спека дозволяє лише цю функцію.
fn json(value: Dynamic) -> Result<Html, Box<EvalAltResult>> {
    let text = serde_json::to_string(&value)
        .map_err(|err| -> Box<EvalAltResult> { format!("json(): {err}").into() })?;
    let mut out = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        match ch {
            '<' => out.push_str("\\u003C"),
            '>' => out.push_str("\\u003E"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            other => out.push(other),
        }
    }
    Ok(Html(out))
}

/// `url("/orders", #{ q: q, page: 2 })` — посилання зі станом.
///
/// Ручна конкатенація ламається на `&` у значенні, тому в доках її немає:
/// параметри кодуються, `()` і порожні рядки пропускаються.
pub(crate) fn url_with_params(path: &str, params: Map) -> String {
    let mut query = String::new();
    for (key, value) in params.iter() {
        if value.is_unit() {
            continue;
        }
        let rendered = super::display(value);
        if rendered.is_empty() {
            continue;
        }
        if !query.is_empty() {
            query.push('&');
        }
        encode_component(key, &mut query);
        query.push('=');
        encode_component(&rendered, &mut query);
    }
    if query.is_empty() {
        return path.to_owned();
    }
    let separator = if path.contains('?') { '&' } else { '?' };
    format!("{path}{separator}{query}")
}

fn url_plain(path: &str) -> String {
    path.to_owned()
}

/// `percent(7, 20)` → `"35%"`. Для смужок у звітах і `@style`.
fn percent(part: i64, total: i64) -> String {
    if total == 0 {
        return "0%".to_owned();
    }
    let value = (part as f64) * 100.0 / (total as f64);
    format!("{}%", value.round() as i64)
}

fn encode_component(text: &str, out: &mut String) {
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{engine, Limits};

    fn eval(script: &str) -> String {
        let mut engine = engine(Limits::default());
        register_core(&mut engine);
        let value = engine
            .eval::<Dynamic>(script)
            .expect("скрипт має працювати");
        if let Some(html) = value.read_lock::<Html>() {
            return html.0.clone();
        }
        crate::display(&value)
    }

    #[test]
    fn negation_and_bool_follow_rhaix_truthiness() {
        // те, що приїхало з SQLite як 0/1, поводиться як булеве
        assert_eq!(eval("!0"), "true");
        assert_eq!(eval("!1"), "");
        assert_eq!(eval("bool(0)"), "");
        assert_eq!(eval("bool(2)"), "true");
        assert_eq!(eval(r#"bool("")"#), "");
        assert_eq!(eval(r#"bool("текст")"#), "true");
    }

    #[test]
    fn trim_returns_the_trimmed_value() {
        // У Rhai `trim()` мутує на місці й повертає (); наша версія повертає
        // результат, інакше `let t = s.trim();` тихо давало б порожнє значення.
        assert_eq!(eval(r#"let s = "  текст  "; s.trim()"#), "текст");
        assert_eq!(eval(r#"let s = "  текст  "; s.trim(); s"#), "текст");
    }

    #[test]
    fn raw_marks_value_as_ready_html() {
        assert_eq!(eval(r#"raw("<b>hi</b>")"#), "<b>hi</b>");
        assert_eq!(eval(r#"raw(raw("<b>hi</b>"))"#), "<b>hi</b>");
    }

    #[test]
    fn json_escapes_what_could_close_a_script_tag() {
        let out = eval(r#"json("</script><img src=x onerror=alert(1)>")"#);
        assert!(!out.contains("</script>"), "{out}");
        assert!(out.contains("\\u003C"), "{out}");
    }

    #[test]
    fn json_serializes_maps_and_arrays() {
        assert_eq!(eval("json(#{ a: 1 })"), r#"{"a":1}"#);
        assert_eq!(eval("json([1, 2])"), "[1,2]");
    }

    #[test]
    fn url_encodes_parameters() {
        assert_eq!(
            eval(r#"url("/orders", #{ q: "a&b=c", page: 2 })"#),
            "/orders?page=2&q=a%26b%3Dc"
        );
    }

    #[test]
    fn url_skips_empty_values() {
        assert_eq!(
            eval(r#"url("/orders", #{ q: "", page: 1 })"#),
            "/orders?page=1"
        );
        assert_eq!(eval(r#"url("/orders", #{})"#), "/orders");
    }

    #[test]
    fn url_appends_to_existing_query() {
        assert_eq!(eval(r#"url("/o?a=1", #{ b: 2 })"#), "/o?a=1&b=2");
    }

    #[test]
    fn percent_rounds_and_survives_zero() {
        assert_eq!(eval("percent(7, 20)"), "35%");
        assert_eq!(eval("percent(1, 0)"), "0%");
    }
}
