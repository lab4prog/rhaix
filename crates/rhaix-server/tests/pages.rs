//! Наскрізні перевірки: справжній роутер, справжні заголовки, справжні `.rhx`.
//!
//! Застосунок для тестів лежить у `tests/fixture` — окремо від демо, щоб
//! перевірки не залежали від того, що зараз показує демо.

use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rhaix_server::{build, Config};
use tower::ServiceExt;

fn app() -> axum::Router {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixture");
    // `Config::load` читає `rhaix.toml` фікстури: база `:memory:` створюється
    // заново на кожен виклик, тож тести не залежать одне від одного.
    let config = Config::load(root, Some(0)).expect("конфіг фікстури");
    build(config)
        .expect("застосунок для тестів має збиратись")
        .0
}

async fn call(request: Request<Body>) -> (StatusCode, Vec<(String, String)>, String) {
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
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn get(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .body(Body::empty())
        .expect("запит")
}

fn htmx(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header("HX-Request", "true")
        .body(Body::empty())
        .expect("запит")
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

#[tokio::test]
async fn full_load_wraps_the_page_in_the_layout() {
    let (status, headers, body) = call(get("/")).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.starts_with("<!DOCTYPE html>"), "{body}");
    // page.title зі сторінки видно в layout — саме тому layout рендериться після неї
    assert!(body.contains("<title>rhaix — Головна</title>"), "{body}");
    assert!(body.contains("<li>перше</li><li>друге</li>"), "{body}");
    // статика підхоплюється сама
    assert!(
        body.contains(r#"<link rel="stylesheet" href="/style.css">"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<script src="/app.js"></script>"#),
        "{body}"
    );
    assert_eq!(header(&headers, "vary"), Some("HX-Request"));
}

#[tokio::test]
async fn htmx_request_gets_only_the_fragment() {
    let (status, headers, body) = call(htmx("/")).await;

    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("<!DOCTYPE html>"), "{body}");
    assert!(body.contains("<h1>Головна</h1>"), "{body}");
    assert_eq!(header(&headers, "cache-control"), Some("private, no-store"));
}

#[tokio::test]
async fn dynamic_segment_reaches_the_script() {
    let (status, _, body) = call(htmx("/item/42")).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("id = 42"), "{body}");
}

#[tokio::test]
async fn form_post_runs_the_logic_and_sends_triggers() {
    let request = Request::builder()
        .method("POST")
        .uri("/form")
        .header("HX-Request", "true")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from("title=%D0%9F%D1%80%D0%B8%D0%B2%D1%96%D1%82"))
        .expect("запит");
    let (status, headers, body) = call(request).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<b>Привіт</b>"), "{body}");
    let trigger = header(&headers, "hx-trigger").expect("є HX-Trigger");
    assert!(trigger.contains(r#""showToast""#), "{trigger}");
    // не-ASCII у заголовку неприпустимий, тому кирилиця їде як \uXXXX
    assert!(trigger.is_ascii(), "{trigger}");
    assert!(trigger.contains("\\u041f\\u0440\\u0438"), "{trigger}");
    assert!(trigger.contains(r#""saved":true"#), "{trigger}");
}

#[tokio::test]
async fn validation_keeps_the_page_but_changes_the_status() {
    let request = Request::builder()
        .method("POST")
        .uri("/form")
        .header("HX-Request", "true")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from("title=%20%20"))
        .expect("запит");
    let (status, headers, body) = call(request).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("порожньо"), "{body}");
    assert!(
        header(&headers, "hx-trigger").is_none(),
        "тостів бути не має"
    );
}

#[tokio::test]
async fn redirect_differs_for_htmx_and_for_a_normal_visit() {
    let (status, headers, body) = call(get("/guard")).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(header(&headers, "location"), Some("/"));
    assert!(body.is_empty(), "розмітка не рендериться: {body}");

    let (status, headers, _) = call(htmx("/guard")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "hx-redirect"), Some("/"));
}

#[tokio::test]
async fn returned_value_becomes_the_body() {
    let request = Request::builder()
        .method("DELETE")
        .uri("/fragment")
        .header("HX-Request", "true")
        .body(Body::empty())
        .expect("запит");
    let (status, _, body) = call(request).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body, "",
        "`return \"\";` віддає порожнє тіло — htmx прибере елемент"
    );
}

#[tokio::test]
async fn script_error_is_shown_in_rhx_coordinates() {
    let (status, _, body) = call(htmx("/broken")).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    // ворота M2: файл, рядок, колонка — і жодного сирого Rhai
    assert!(body.contains("pages/broken.rhx:3:"), "{body}");
    assert!(body.contains("missing_variable"), "{body}");
    assert!(!body.contains("EvalAltResult"), "{body}");
}

// ------------------------------------------------------------- компоненти

#[tokio::test]
async fn components_render_with_props_and_slots() {
    let (status, _, body) = call(htmx("/card")).await;

    assert_eq!(status, StatusCode::OK);
    // іменований слот, props як змінні, булевий prop, вкладений компонент
    assert!(body.contains("<header><h2>Картка</h2></header>"), "{body}");
    assert!(body.contains(r#"<p class="greeting loud">"#), "{body}");
    assert!(body.contains("Привіт, рhaix!"), "{body}");
    assert!(body.contains("і слот теж"), "{body}");
    assert!(
        !body.contains("без додатку"),
        "слот передано — запасний вміст не потрібен"
    );
}

#[tokio::test]
async fn component_does_not_see_page_variables() {
    // Ізоляція scope (SYNTAX 5.3): змінна є на сторінці, але компонент її не
    // бачить — і дізнається про це зрозумілою помилкою, а не порожнім місцем.
    let (status, _, body) = call(htmx("/secret")).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(body.contains("невідома змінна `secret`"), "{body}");
    assert!(body.contains("components/Nosy.rhx:1:"), "{body}");
    assert!(body.contains("у ланцюжку"), "{body}");
}

#[tokio::test]
async fn unknown_component_suggests_a_similar_name() {
    let (status, _, body) = call(htmx("/typo")).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(body.contains("не знайдено"), "{body}");
    assert!(body.contains("Greeting"), "підказка про схоже ім'я: {body}");
}

#[tokio::test]
async fn component_cycle_is_caught_before_rendering() {
    // Node-RED-стартер ловив це лімітом рекурсії під час запиту; тут цикл
    // виявляється при компіляції, разом із повним ланцюжком.
    let (status, _, body) = call(htmx("/cycle")).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(body.contains("циклічна залежність"), "{body}");
    assert!(body.contains("ланцюжок"), "{body}");
}

// ------------------------------------------------ маршрути, middleware, HTMX

#[tokio::test]
async fn middleware_guards_a_page() {
    // Охорона живе в одному файлі, а не копіюється в кожну закриту сторінку.
    let (status, headers, body) = call(get("/closed")).await;

    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(header(&headers, "location"), Some("/"));
    assert!(body.is_empty(), "сторінка не виконується: {body}");
}

#[tokio::test]
async fn middleware_runs_before_the_page() {
    let (status, _, body) = call(htmx("/traced")).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("middleware виконався: true"), "{body}");
}

#[tokio::test]
async fn partials_are_fragments_even_without_hx_request() {
    // `partials/Row.rhx` → `/components/row`, layout не додається ніколи:
    // це прямий аналог `/components/todo` з Node-RED-стартера.
    let (status, _, body) = call(get("/components/row")).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        !body.contains("<!DOCTYPE html>"),
        "фрагмент без layout: {body}"
    );
    assert!(body.contains("рядок із partials/"), "{body}");
}

#[tokio::test]
async fn oob_directive_produces_an_out_of_band_fragment() {
    let (status, _, body) = call(htmx("/oob")).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains(r#"<span id="side" hx-swap-oob="outerHTML:#side">збоку</span>"#),
        "{body}"
    );
}

#[tokio::test]
async fn attribute_directives_on_a_component_are_rejected() {
    // Мовчки проігнорована директива — найгірший варіант: людина бачить, що
    // клас не застосувався, і не розуміє чому.
    let (status, _, body) = call(htmx("/badcomp")).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(body.contains("компонент"), "{body}");
    assert!(body.contains("props"), "підказка про props: {body}");
}

// -------------------------------------------------------------------- дані

#[tokio::test]
async fn pages_read_from_the_database() {
    // Схема приїхала з `migrations/001_notes.sql` на старті застосунку.
    let (status, _, body) = call(htmx("/notes")).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<li>перша</li>"), "{body}");
    assert!(body.contains(r#"<li class="done">друга</li>"#), "{body}");
    assert!(body.contains("зроблено: 1 із 2"), "{body}");
}

#[tokio::test]
async fn pages_write_to_the_database() {
    let request = Request::builder()
        .method("POST")
        .uri("/notes")
        .header("HX-Request", "true")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from("title=%D1%82%D1%80%D0%B5%D1%82%D1%8F"))
        .expect("запит");
    let (status, headers, body) = call(request).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("третя"), "{body}");
    assert!(body.contains("зроблено: 1 із 3"), "{body}");
    assert!(
        header(&headers, "hx-trigger").is_some(),
        "тост про додавання"
    );
}

#[tokio::test]
async fn database_errors_point_at_the_rhx_file() {
    let (status, _, body) = call(htmx("/dbfail")).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(body.contains("pages/dbfail.rhx:2:"), "{body}");
    assert!(body.contains("no_such_table"), "{body}");
}

// ------------------------------------------------------------------ асети

#[tokio::test]
async fn component_styles_and_scripts_are_hoisted_once() {
    let (status, _, body) = call(get("/assets")).await;
    assert_eq!(status, StatusCode::OK);

    // Компонент ужито двічі — стиль і скрипт по одному разу.
    assert_eq!(body.matches("<style data-rhx=").count(), 1, "{body}");
    assert_eq!(body.matches("<script data-rhx=").count(), 1, "{body}");

    // Стиль — у <head>, скрипт — після розмітки.
    let head_end = body.find("</head>").expect("є head");
    let style_at = body.find("<style data-rhx=").expect("є стиль");
    let script_at = body.find("<script data-rhx=").expect("є скрипт");
    assert!(style_at < head_end, "стиль має бути в head");
    assert!(script_at > head_end, "скрипт — нижче");

    // `rhaix.js` іде раніше за піднятий скрипт: той питає в нього реєстр.
    let client_at = body.find("/_rhaix/rhaix.js").expect("є rhaix.js");
    assert!(client_at < script_at, "{body}");
}

#[tokio::test]
async fn fragments_carry_their_assets_with_a_guard() {
    let (status, _, body) = call(htmx("/assets")).await;
    assert_eq!(status, StatusCode::OK);

    assert!(!body.contains("<!DOCTYPE html>"), "це фрагмент: {body}");
    assert!(body.contains("<style data-rhx="), "{body}");
    // Скрипт у фрагменті загорнутий у перевірку реєстру, інакше htmx виконував
    // би його на кожному свопі.
    assert!(body.contains("__rhaix.seen("), "{body}");
}

#[tokio::test]
async fn the_client_script_is_served_by_the_core() {
    let (status, headers, body) = call(get("/_rhaix/rhaix.js")).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        header(&headers, "content-type")
            .unwrap_or_default()
            .contains("javascript"),
        "{headers:?}"
    );
    assert!(body.contains("showToast"), "тости з коробки");
    assert!(body.contains("window.__rhaix"), "реєстр асетів");
}

#[tokio::test]
async fn missing_page_is_a_404() {
    let (status, _, _) = call(get("/nope")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
