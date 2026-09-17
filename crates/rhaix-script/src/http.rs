//! `http` — виклик зовнішнього API прямо з frontmatter.
//!
//! Виклик синхронний, і це навмисно: сторінка виконується в `spawn_blocking`,
//! тож для користувача рядок `let rate = http.get(url).json;` читається саме
//! так, як виглядає. Ніякого `await`, ніяких промісів.
//!
//! **Помилка мережі не валить сторінку.** API впав — повертається мапа з
//! `ok: false` і `error`, а не виняток. Інакше кожен виклик довелось би
//! обгортати перевіркою, а людина, яка не програміст, цього не зробить:
//! сторінка просто показала б 500 замість «курс тимчасово недоступний».

use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rhai::{Dynamic, Engine, Map};

use crate::json;

/// Скільки чекати на відповідь, якщо не сказано інакше.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// Скільки тіла читаємо: відповідь API — це JSON, а не файл на гігабайт.
const MAX_BODY: usize = 8 * 1024 * 1024;

/// `http` у скрипті.
#[derive(Clone)]
pub struct Http {
    agent: Arc<ureq::Agent>,
}

impl std::fmt::Debug for Http {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Http")
    }
}

impl Default for Http {
    fn default() -> Self {
        Self::new(DEFAULT_TIMEOUT)
    }
}

impl Http {
    pub fn new(timeout: Duration) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(timeout)
            .user_agent(concat!("rhaix/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: Arc::new(agent),
        }
    }

    fn send(&self, method: &str, url: &str, body: Option<Dynamic>, headers: Option<Map>) -> Map {
        if let Some(problem) = bad_url(url) {
            return failure(problem);
        }

        let mut request = self.agent.request(method, url);
        let mut has_content_type = false;
        if let Some(headers) = &headers {
            for (name, value) in headers {
                if name.eq_ignore_ascii_case("content-type") {
                    has_content_type = true;
                }
                request = request.set(name, &crate::display(value));
            }
        }

        // Мапа або масив їдуть як JSON — це те, чого чекають від сучасного API.
        // Рядок їде як є: якщо треба form-urlencoded, людина сама поставить
        // заголовок і склеїть рядок.
        let payload = match &body {
            None => None,
            Some(value) if value.is_unit() => None,
            Some(value) if value.is_map() || value.is_array() => {
                if !has_content_type {
                    request = request.set("Content-Type", "application/json");
                }
                Some(json::from_dynamic(value).to_string())
            }
            Some(value) => Some(crate::display(value)),
        };

        // Мережа — це не робота скрипта. Час, проведений у очікуванні,
        // повертаємо дедлайну, інакше повільний API «з'їдав» би бюджет
        // сторінки й вона падала б уже після успішної відповіді.
        let started = Instant::now();
        let outcome = match payload {
            Some(text) => request.send_string(&text),
            None => request.call(),
        };
        crate::extend_deadline(started.elapsed());

        match outcome {
            Ok(response) => read_response(response),
            // 4xx/5xx — це відповідь, а не аварія: статус і тіло потрібні
            // рівно так само, як при 200.
            Err(ureq::Error::Status(_, response)) => read_response(response),
            Err(ureq::Error::Transport(transport)) => failure(transport.to_string()),
        }
    }
}

fn read_response(response: ureq::Response) -> Map {
    let status = response.status() as i64;
    let mut headers = Map::new();
    for name in response.headers_names() {
        if let Some(value) = response.header(&name) {
            headers.insert(name.to_ascii_lowercase().into(), Dynamic::from(value.to_owned()));
        }
    }

    let mut body = String::new();
    let read = response
        .into_reader()
        .take(MAX_BODY as u64)
        .read_to_string(&mut body);

    let mut result = Map::new();
    result.insert("status".into(), Dynamic::from(status));
    result.insert("ok".into(), Dynamic::from((200..300).contains(&status)));
    result.insert("headers".into(), Dynamic::from_map(headers));
    match read {
        Ok(_) => {
            // `json` заповнюється, лише якщо тіло справді розбирається:
            // HTML-сторінка помилки не має вдавати об'єкт.
            result.insert(
                "json".into(),
                json::parse(&body).unwrap_or(Dynamic::UNIT),
            );
            result.insert("body".into(), Dynamic::from(body));
            result.insert("error".into(), Dynamic::UNIT);
        }
        Err(err) => {
            result.insert("json".into(), Dynamic::UNIT);
            result.insert("body".into(), Dynamic::from(String::new()));
            result.insert("error".into(), Dynamic::from(err.to_string()));
        }
    }
    result
}

fn failure(message: String) -> Map {
    let mut result = Map::new();
    result.insert("status".into(), Dynamic::from(0_i64));
    result.insert("ok".into(), Dynamic::from(false));
    result.insert("headers".into(), Dynamic::from_map(Map::new()));
    result.insert("body".into(), Dynamic::from(String::new()));
    result.insert("json".into(), Dynamic::UNIT);
    result.insert("error".into(), Dynamic::from(message));
    result
}

/// Єдина перевірка адреси: схема.
///
/// `file://` і подібне з `http.get()` нікому не потрібне, а от підставити таку
/// адресу з поля форми — цілком реальний сценарій.
fn bad_url(url: &str) -> Option<String> {
    let lower = url.trim().to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return None;
    }
    Some(format!(
        "`{url}` — http приймає лише адреси http:// і https://"
    ))
}

pub fn register_http(engine: &mut Engine) {
    engine.register_type_with_name::<Http>("Http");

    for method in ["get", "delete", "head"] {
        let verb = method.to_ascii_uppercase();
        let with_headers = verb.clone();
        engine.register_fn(method, move |http: &mut Http, url: &str| {
            Dynamic::from_map(http.send(&verb, url, None, None))
        });
        engine.register_fn(method, move |http: &mut Http, url: &str, headers: Map| {
            Dynamic::from_map(http.send(&with_headers, url, None, Some(headers)))
        });
    }

    for method in ["post", "put", "patch"] {
        let verb = method.to_ascii_uppercase();
        let with_headers = verb.clone();
        let bare = verb.clone();
        engine.register_fn(method, move |http: &mut Http, url: &str| {
            Dynamic::from_map(http.send(&bare, url, None, None))
        });
        engine.register_fn(method, move |http: &mut Http, url: &str, body: Dynamic| {
            Dynamic::from_map(http.send(&verb, url, Some(body), None))
        });
        engine.register_fn(
            method,
            move |http: &mut Http, url: &str, body: Dynamic, headers: Map| {
                Dynamic::from_map(http.send(&with_headers, url, Some(body), Some(headers)))
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrong_scheme_never_reaches_the_network() {
        let http = Http::default();
        let result = http.send("GET", "file:///etc/passwd", None, None);
        assert!(!result["ok"].clone().cast::<bool>());
        assert_eq!(result["status"].clone().cast::<i64>(), 0);
        assert!(result["error"]
            .clone()
            .into_string()
            .unwrap()
            .contains("http://"));
    }

    #[test]
    fn unreachable_host_returns_a_map_not_a_panic() {
        // Порт 9 — discard; з'єднання або відмовлять, або впадуть за таймаутом.
        let http = Http::new(Duration::from_millis(400));
        let result = http.send("GET", "http://127.0.0.1:9/nope", None, None);
        assert!(!result["ok"].clone().cast::<bool>());
        assert!(!result["error"].is_unit(), "помилка має бути описана");
        assert_eq!(result["body"].clone().into_string().unwrap(), "");
    }

    #[test]
    fn failure_map_has_every_field_a_page_might_read() {
        // Сторінка не має падати на `.json`, `.headers` чи `.status` після збою.
        let result = failure("тест".into());
        for key in ["status", "ok", "headers", "body", "json", "error"] {
            assert!(result.contains_key(key), "немає `{key}`");
        }
    }
}
