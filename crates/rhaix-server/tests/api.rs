//! Маршрути з `api/`: JSON замість HTML, без layout, без сесії й без CSRF.
//!
//! Перевірки тут навмисно дублюють те, що вже сказано в `pages.rs` про HTML:
//! саме розбіжність між «сторінка віддає одне, API — інше» і є те, що ламається
//! непомітно.

use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rhaix_server::{build, Config};
use serde_json::Value;
use tower::ServiceExt;

fn app() -> axum::Router {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixture");
    let config = Config::load(root, Some(0)).expect("конфіг фікстури");
    build(config)
        .expect("застосунок для тестів має збиратись")
        .0
}

struct Reply {
    status: StatusCode,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    /// Тіло, розібране справжнім JSON-парсером. Саме в цьому суть перевірки:
    /// рядок, «схожий на JSON», тут не пройде.
    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|err| panic!("тіло не JSON ({err}): {}", self.body))
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

async fn call(request: Request<Body>) -> Reply {
    let response = app().oneshot(request).await.expect("запит має оброблятись");
    let status = response.status();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                value.to_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("тіло має читатись");
    Reply {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn get(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .body(Body::empty())
        .expect("запит")
}

fn post(path: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_owned()))
        .expect("запит")
}

#[tokio::test]
async fn a_map_becomes_real_json() {
    let reply = call(get("/api/ping")).await;

    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.header("content-type"),
        Some("application/json; charset=utf-8")
    );
    // `return #{ ... }` без `json_encode` — і це справжній JSON, а не `#{"ok": true}`.
    assert_eq!(reply.json()["ok"], Value::Bool(true));
    assert_eq!(reply.json()["method"], "GET");
}

#[tokio::test]
async fn no_layout_and_no_assets_are_glued_on() {
    let reply = call(get("/api/ping")).await;

    assert!(!reply.body.contains("<!DOCTYPE"), "{}", reply.body);
    assert!(!reply.body.contains("<style"), "{}", reply.body);
    assert!(!reply.body.contains("<script"), "{}", reply.body);
    assert!(reply.body.starts_with('{'), "{}", reply.body);
}

#[tokio::test]
async fn a_write_needs_no_csrf_token() {
    // Рівно те, що до M14 віддавало 403: POST зовнішнього клієнта без cookie.
    let reply = call(post("/api/echo", r#"{"name":"тест"}"#)).await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.json()["got"], "тест");
    assert_eq!(reply.json()["upper"], "ТЕСТ");
}

#[tokio::test]
async fn a_form_post_still_needs_its_token() {
    // Захист HTML-частини не ослаб: `api/` не відкрив дірку в `pages/`.
    let request = Request::builder()
        .method("POST")
        .uri("/form")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from("title=щось"))
        .expect("запит");
    let reply = call(request).await;

    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.body);
}

#[tokio::test]
async fn broken_body_is_the_scripts_business_not_a_crash() {
    let reply = call(post("/api/echo", "це не json")).await;

    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.json()["error"], "тіло не JSON");
}

#[tokio::test]
async fn route_params_work_as_in_pages() {
    let reply = call(get("/api/orders/42")).await;

    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json()["id"], "42");
    assert_eq!(reply.json()["kind"], "order");
}

#[tokio::test]
async fn an_unknown_api_path_answers_in_json() {
    // Клієнт, що отримує HTML на 404, падає з помилкою розбору замість
    // пояснення — саме цього ми й уникаємо.
    let reply = call(get("/api/nothing/here")).await;

    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(
        reply.header("content-type"),
        Some("application/json; charset=utf-8")
    );
    assert!(reply.json()["error"].is_string(), "{}", reply.body);
}

#[tokio::test]
async fn a_script_error_is_json_too() {
    let reply = call(get("/api/boom")).await;

    assert_eq!(reply.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        reply.header("content-type"),
        Some("application/json; charset=utf-8")
    );
    let message = reply.json()["error"].as_str().unwrap_or("").to_owned();
    // Діагностика лишається в координатах `.rhx`, просто доїжджає полем JSON.
    assert!(message.contains("no_such_function"), "{message}");
}

#[tokio::test]
async fn api_never_sees_a_session_even_with_a_valid_cookie() {
    // Беремо справжній cookie сесії зі сторінки з формою і підсовуємо його в API.
    let form = call(
        Request::builder()
            .uri("/form")
            .header("HX-Request", "true")
            .body(Body::empty())
            .expect("запит"),
    )
    .await;
    let cookie = form
        .header("set-cookie")
        .expect("сторінка з формою має видати cookie")
        .split(';')
        .next()
        .expect("значення cookie")
        .to_owned();

    let reply = call(
        Request::builder()
            .uri("/api/whoami")
            .header("cookie", &cookie)
            .body(Body::empty())
            .expect("запит"),
    )
    .await;

    // Сесія порожня, хоча cookie дійсний: інакше `api/` без CSRF став би
    // каналом для запитів від імені залогіненого користувача.
    assert!(reply.json()["user"].is_null(), "{}", reply.body);
    // І назад свою сесію API теж не віддає.
    assert_eq!(reply.header("set-cookie"), None, "{:?}", reply.headers);
}

// ------------------------------------------------- рецепт із кухарської книги

/// Кухарська книга з базою в пам'яті: міграції застосовуються заново на кожен
/// виклик, тож тести не залишають по собі файл і не залежать одне від одного.
fn cookbook() -> axum::Router {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/cookbook");
    let mut config = Config::load(root, Some(0)).expect("конфіг кухарської книги");
    if let Some(database) = config.database.as_mut() {
        database.url = ":memory:".to_owned();
    }
    build(config).expect("кухарська книга має збиратись").0
}

async fn cook(request: Request<Body>) -> Reply {
    let response = cookbook()
        .oneshot(request)
        .await
        .expect("запит має оброблятись");
    let status = response.status();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                value.to_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("тіло має читатись");
    Reply {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

/// Токен із міграції `002_api_tokens.sql`.
const TOKEN: &str = "demo-token-42";

fn authed(method: &str, path: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_owned()))
        .expect("запит")
}

#[tokio::test]
async fn the_recipe_rejects_a_request_without_a_token() {
    let reply = cook(get("/api/orders")).await;

    assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.body);
    assert!(reply.json()["error"].is_string(), "{}", reply.body);
}

#[tokio::test]
async fn the_recipe_rejects_a_wrong_token() {
    let request = Request::builder()
        .uri("/api/orders")
        .header("authorization", "Bearer не-той-токен")
        .body(Body::empty())
        .expect("запит");
    let reply = cook(request).await;

    assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.body);
}

#[tokio::test]
async fn the_recipe_lists_orders_with_a_token() {
    let reply = cook(authed("GET", "/api/orders", "")).await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let data = reply.json();
    assert!(data["data"].as_array().expect("масив").len() >= 4, "{data}");
    assert_eq!(data["page"]["number"], 1);
    assert!(data["page"]["total"].as_i64().unwrap_or(0) >= 4, "{data}");
}

#[tokio::test]
async fn the_recipe_validates_before_it_writes() {
    let reply = cook(authed(
        "POST",
        "/api/orders",
        r#"{"customer":"я","email":"не пошта","amount":0}"#,
    ))
    .await;

    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", reply.body);
    let errors = &reply.json()["errors"];
    assert!(errors["customer"].is_string(), "{errors}");
    assert!(errors["email"].is_string(), "{errors}");
    assert!(errors["amount"].is_string(), "{errors}");
}

#[tokio::test]
async fn the_recipe_creates_and_returns_the_record() {
    let reply = cook(authed(
        "POST",
        "/api/orders",
        r#"{"customer":"Нова Клієнтка","email":"n@example.com","amount":250.5}"#,
    ))
    .await;

    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let created = reply.json();
    assert_eq!(created["customer"], "Нова Клієнтка");
    assert_eq!(created["status"], "new");
    assert!(created["id"].as_i64().unwrap_or(0) > 0, "{created}");
}

#[tokio::test]
async fn the_recipe_reads_one_record_by_id() {
    let reply = cook(authed("GET", "/api/orders/1", "")).await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.json()["id"], 1);
}

#[tokio::test]
async fn a_missing_record_is_a_json_404() {
    let reply = cook(authed("GET", "/api/orders/9999", "")).await;

    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.body);
    assert!(reply.json()["error"].is_string(), "{}", reply.body);
}

#[tokio::test]
async fn a_patch_changes_only_the_fields_that_were_sent() {
    let reply = cook(authed("PATCH", "/api/orders/1", r#"{"status":"cancelled"}"#)).await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let updated = reply.json();
    assert_eq!(updated["status"], "cancelled");
    // Клієнт не надсилали — він має лишитись тим самим.
    assert_eq!(updated["customer"], "Оксана Литвин");
}

#[tokio::test]
async fn a_patch_with_an_unknown_status_is_refused() {
    let reply = cook(authed("PATCH", "/api/orders/1", r#"{"status":"вигадка"}"#)).await;

    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", reply.body);
    assert!(reply.json()["errors"]["status"].is_string(), "{}", reply.body);
}

#[tokio::test]
async fn a_patch_cannot_smuggle_a_column_that_has_no_rule() {
    // `id` у дозволених полях немає, тож зміна має бути відкинута як порожня.
    let reply = cook(authed("PATCH", "/api/orders/1", r#"{"id":777}"#)).await;

    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
}

#[tokio::test]
async fn a_delete_answers_without_a_body() {
    let reply = cook(authed("DELETE", "/api/orders/2", "")).await;

    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    assert!(reply.body.is_empty(), "204 не має тіла: {}", reply.body);
}

#[tokio::test]
async fn the_html_recipes_still_work_without_any_token() {
    // Охорона в middleware стосується лише `/api/`: сторінки лишились відкриті.
    let reply = cook(get("/pagination")).await;

    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.contains("<!DOCTYPE html>"), "{}", reply.body);
}
