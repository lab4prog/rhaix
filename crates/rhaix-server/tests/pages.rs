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

/// Cookie сесії й CSRF-токен: беремо їх так само, як браузер — з відповіді
/// на звичайний перегляд сторінки з формою.
///
/// Токен живе в сесії, а не в сторінці, тому одного квитка вистачає для
/// будь-якого маршруту.
async fn ticket() -> (String, String) {
    let (_, headers, html) = call(htmx("/form")).await;
    let cookie = header(&headers, "set-cookie")
        .expect("сторінка з формою має видати cookie сесії")
        .split(';')
        .next()
        .expect("значення cookie")
        .to_owned();

    let marker = "name=\"_csrf\" value=\"";
    let start = html.find(marker).expect("у формі має бути приховане поле") + marker.len();
    let token = html[start..]
        .split('"')
        .next()
        .expect("значення токена")
        .to_owned();
    (cookie, token)
}

/// POST формою — з cookie й токеном, як це робить браузер.
fn form_post(path: &str, body: &str, ticket: &(String, String)) -> Request<Body> {
    let (cookie, token) = ticket;
    Request::builder()
        .method("POST")
        .uri(path)
        .header("HX-Request", "true")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("cookie", cookie)
        .body(Body::from(format!("{body}&_csrf={token}")))
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
    let ticket = ticket().await;
    let request = form_post("/form", "title=%D0%9F%D1%80%D0%B8%D0%B2%D1%96%D1%82", &ticket);
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
    let ticket = ticket().await;
    let request = form_post("/form", "title=%20%20", &ticket);
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
    let (cookie, token) = ticket().await;
    let request = Request::builder()
        .method("DELETE")
        .uri("/fragment")
        .header("HX-Request", "true")
        .header("cookie", cookie)
        // Кнопці прихованого поля нема де взяти — токен їде заголовком,
        // так само, як його ставить `hx-headers`.
        .header("x-csrf-token", token)
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
    let ticket = ticket().await;
    let request = form_post("/notes", "title=%D1%82%D1%80%D0%B5%D1%82%D1%8F", &ticket);
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

// ----------------------------------------------------------- вшитий режим

/// Застосунок, файли якого існують лише в пам'яті — так само, як у бінарнику
/// після `rhaix build`.
fn embedded_app() -> axum::Router {
    let files = rhaix_template::EmbeddedFiles::new(&[
        (
            "rhaix.toml",
            b"[db]
driver = \"sqlite\"
url = \":memory:\"
",
        ),
        (
            "layouts/main.rhx",
            b"<!DOCTYPE html><html><head><rhaix:head /></head><body><slot /></body></html>",
        ),
        ("pages/index.rhx", "<h1>вшито</h1><Mark />".as_bytes()),
        ("components/Mark.rhx", "<b>компонент теж</b>".as_bytes()),
        ("public/style.css", b"body{margin:0}"),
        (
            "migrations/001_init.sql",
            b"create table notes (id integer primary key, title text)",
        ),
    ]);
    let config = rhaix_server::Config::embedded(files, Some(0)).expect("вшитий конфіг");
    build(config).expect("вшитий застосунок").0
}

async fn call_on(router: axum::Router, request: Request<Body>) -> (StatusCode, String) {
    let response = router.oneshot(request).await.expect("запит");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("тіло");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn embedded_app_serves_pages_components_and_assets() {
    let (status, body) = call_on(embedded_app(), get("/")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<h1>вшито</h1>"), "{body}");
    assert!(
        body.contains("<b>компонент теж</b>"),
        "компонент з пам'яті: {body}"
    );
    assert!(
        body.contains("<link rel=\"stylesheet\" href=\"/style.css\">"),
        "{body}"
    );

    let (status, body) = call_on(embedded_app(), get("/style.css")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("margin"), "статика теж із пам'яті: {body}");
}

#[tokio::test]
async fn embedded_app_has_no_dev_client() {
    let (_, body) = call_on(embedded_app(), get("/")).await;
    assert!(
        !body.contains("_rhaix/events"),
        "у продакшні перезавантаження немає"
    );
}

#[tokio::test]
async fn missing_page_is_a_404() {
    let (status, _, _) = call(get("/nope")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// --------------------------------------------------- сесія, CSRF, утиліти

#[tokio::test]
async fn mutating_request_without_a_token_is_refused() {
    let request = Request::builder()
        .method("POST")
        .uri("/form")
        .header("HX-Request", "true")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from("title=%D0%B7%D0%BB%D0%BE"))
        .expect("запит");
    let (status, _, body) = call(request).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("Оновіть сторінку"), "{body}");
}

#[tokio::test]
async fn a_stolen_cookie_without_the_matching_token_is_refused() {
    // Класична CSRF-атака: cookie браузер підставить сам, а токена в чужого
    // сайту немає.
    let (cookie, _) = ticket().await;
    let request = Request::builder()
        .method("POST")
        .uri("/form")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("cookie", cookie)
        .body(Body::from("title=%D0%B7%D0%BB%D0%BE"))
        .expect("запит");
    let (status, _, _) = call(request).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_page_without_forms_sets_no_cookie() {
    // Токен — лінивий: сторінка, яка нічого не змінює, не має тягти за собою
    // ні сесію, ні `Set-Cookie`.
    let (status, headers, _) = call(htmx("/public")).await;

    assert_eq!(status, StatusCode::OK);
    assert!(header(&headers, "set-cookie").is_none());
}

#[tokio::test]
async fn forms_carry_a_hidden_field_and_buttons_carry_a_header() {
    let (_, _, html) = call(htmx("/account")).await;

    assert!(html.contains(r#"<input type="hidden" name="_csrf" value=""#), "{html}");
    // Кнопка з `hx-delete` не має форми, тому токен їде заголовком.
    assert!(html.contains("hx-headers='{&quot;x-csrf-token&quot;"), "{html}");
}

#[tokio::test]
async fn session_survives_between_requests() {
    let ticket = ticket().await;
    let (status, headers, body) = call(form_post("/account", "name=%D0%9E%D0%BB%D1%8F", &ticket)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("привіт, Оля"), "{body}");

    // Другий запит несе cookie, виданий першим — і застосунок пам'ятає, хто це.
    let cookie = header(&headers, "set-cookie")
        .expect("сесію змінили — має бути cookie")
        .split(';')
        .next()
        .expect("значення")
        .to_owned();
    let request = Request::builder()
        .uri("/account")
        .header("HX-Request", "true")
        .header("cookie", &cookie)
        .body(Body::empty())
        .expect("запит");
    let (_, _, body) = call(request).await;
    assert!(body.contains("привіт, Оля"), "{body}");

    // Вихід гасить cookie, а не лишає порожній конверт.
    let token = ticket.1.clone();
    let request = Request::builder()
        .method("POST")
        .uri("/account")
        .header("HX-Request", "true")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("cookie", &cookie)
        .body(Body::from(format!("logout=1&_csrf={token}")))
        .expect("запит");
    let (_, headers, body) = call(request).await;
    assert!(body.contains("привіт, гість"), "{body}");

    // Cookie після виходу не порожній: форма на тій самій сторінці одразу
    // виписує новий CSRF-токен. Важливо інше — він **інший**, тобто разом із
    // користувачем змінився й токен (захист від фіксації сесії).
    let after = header(&headers, "set-cookie").expect("сесію змінили");
    assert!(!after.contains("Max-Age=0"), "{after}");
    assert_ne!(after.split(';').next(), Some(cookie.as_str()));
}

#[tokio::test]
async fn stdlib_is_available_in_pages() {
    let (status, _, body) = call(htmx("/tools")).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(r#"<p id="date">17.09.2026 14:05</p>"#), "{body}");
    assert!(body.contains(r#"<p id="slug">pryvit-svite</p>"#), "{body}");
    assert!(body.contains("1\u{a0}234,50 грн"), "{body}");
    assert!(body.contains(r#"<p id="cut">один два…</p>"#), "{body}");
    assert!(body.contains(r#"<p id="json">8</p>"#), "{body}");
}

#[tokio::test]
async fn shared_scripts_are_visible_everywhere() {
    // `scripts/helpers.rhai` — єдиний спосіб не копіювати ту саму функцію
    // по десятку сторінок. Підключати нічого не треба, і в `{{ }}` вони
    // працюють так само, як у frontmatter.
    let (status, _, body) = call(htmx("/shared")).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(r#"<p id="vat">120,00</p>"#), "{body}");
    assert!(body.contains(r#"<p id="label">1 запис</p>"#), "{body}");
}

#[tokio::test]
async fn a_transaction_is_all_or_nothing() {
    // У фікстурі дві нотатки з міграції. Успішна транзакція додає дві,
    // зламана — жодної, навіть тієї, що встигла вставитись.
    let (status, _, body) = call(htmx("/tx")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(r#"<p id="failed">ні</p>"#), "{body}");
    assert!(body.contains(r#"<p id="total">4</p>"#), "{body}");

    let (status, _, body) = call(htmx("/tx?mode=fail")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(r#"<p id="failed">так</p>"#), "{body}");
    assert!(
        body.contains(r#"<p id="total">2</p>"#),
        "відкат мав прибрати й перший запис: {body}"
    );
}

#[tokio::test]
async fn only_an_explicit_return_replaces_the_page() {
    // У Rhai значення останнього виразу лишається значенням блоку навіть із
    // крапкою з комою. Через це frontmatter, який закінчувався на
    // `db.insert(...);`, мовчки віддавав клієнту новий id замість сторінки.
    let (status, _, body) = call(htmx("/tail")).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("розмітка на місці"),
        "останній вираз frontmatter не має ставати тілом: {body}"
    );

    // А явний `return` працює як і раніше.
    let (_, _, body) = call(htmx("/tail?mode=return")).await;
    assert_eq!(body, "готове тіло");
}

#[tokio::test]
async fn a_page_can_choose_its_layout() {
    // `page.layout = "print"` → layouts/print.rhx замість main.rhx.
    let (status, _, body) = call(get("/report")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(r#"<body class="print">"#), "{body}");
    assert!(body.contains("<p>звіт</p>"), "{body}");

    // `page.layout = false` → сторінка сама собі документ.
    let (_, _, body) = call(get("/report?bare=1")).await;
    assert!(!body.contains("<!DOCTYPE html>"), "{body}");
    assert!(body.contains("<p>звіт</p>"), "{body}");
}

#[tokio::test]
async fn a_layout_name_cannot_escape_the_layouts_directory() {
    // Ім'я layout приходить зі скрипта користувача, тож воно звіряється:
    // `../../` не має нікуди вести, і сторінка не має падати.
    let (status, _, body) = call(get("/report?layout=..%2F..%2Fmiddleware")).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("<p>звіт</p>"), "{body}");
    // Відкат на layout за замовчуванням, а не на чужий файл.
    assert!(!body.contains(r#"<body class="print">"#), "{body}");
}

#[tokio::test]
async fn passwords_are_hashed_and_verified() {
    // Реєстрація новим іменем, потім вхід тим самим паролем, потім відмова на
    // неправильному — усе через справжній Argon2, з тим самим застосунком, бо
    // база `:memory:` в межах одного `app()` спільна.
    let app = app();

    async fn post(app: &axum::Router, ticket: &(String, String), password: &str) -> (StatusCode, String) {
        let (cookie, token) = ticket;
        let request = Request::builder()
            .method("POST")
            .uri("/auth")
            .header("HX-Request", "true")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("cookie", cookie)
            .body(Body::from(format!("username=oksana&password={password}&_csrf={token}")))
            .expect("запит");
        let response = app.clone().oneshot(request).await.expect("оброблено");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    // Квиток CSRF беремо зі сторінки auth (сесія одна на застосунок).
    let (_, headers, html) = {
        let response = app
            .clone()
            .oneshot(htmx("/auth"))
            .await
            .expect("оброблено");
        let status = response.status();
        assert_eq!(status, StatusCode::OK);
        let headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(n, v)| (n.as_str().to_owned(), v.to_str().unwrap_or_default().to_owned()))
            .collect();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, headers, String::from_utf8_lossy(&bytes).into_owned())
    };
    let cookie = header(&headers, "set-cookie")
        .expect("cookie сесії")
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let marker = "name=\"_csrf\" value=\"";
    let start = html.find(marker).expect("є токен") + marker.len();
    let token = html[start..].split('"').next().unwrap().to_owned();
    let ticket = (cookie, token);

    // Реєстрація.
    let (status, body) = post(&app, &ticket, "правильний123").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(r#"<p id="status">registered</p>"#), "{body}");
    assert!(body.contains(r#"<p id="user">oksana</p>"#), "{body}");
    // У базі — Argon2id, не пароль.
    assert!(body.contains(r#"<p id="hash">$argon2id$</p>"#), "{body}");

    // Вхід правильним паролем.
    let (status, body) = post(&app, &ticket, "правильний123").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(r#"<p id="status">ok</p>"#), "{body}");

    // Відмова на неправильному.
    let (status, body) = post(&app, &ticket, "неправильний").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.contains(r#"<p id="status">wrong</p>"#), "{body}");
}

#[tokio::test]
async fn multipart_upload_is_parsed_and_exposed() {
    // Той самий застосунок: GET дає cookie+токен, POST — multipart із текстовим
    // полем (_csrf) і файлом.
    let app = app();

    let (_, headers, html) = {
        let r = app.clone().oneshot(htmx("/upload")).await.expect("оброблено");
        let hs: Vec<(String, String)> = r
            .headers()
            .iter()
            .map(|(n, v)| (n.as_str().to_owned(), v.to_str().unwrap_or_default().to_owned()))
            .collect();
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        ((), hs, String::from_utf8_lossy(&bytes).into_owned())
    };
    let cookie = header(&headers, "set-cookie")
        .expect("cookie")
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let marker = "name=\"_csrf\" value=\"";
    let start = html.find(marker).expect("токен") + marker.len();
    let token = html[start..].split('"').next().unwrap().to_owned();

    let boundary = "----rhaixTEST";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"_csrf\"\r\n\r\n{token}\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"doc\"; filename=\"нотатка.txt\"\r\n\
         Content-Type: text/plain\r\n\r\nвміст файлу\r\n--{boundary}--\r\n"
    );
    let request = Request::builder()
        .method("POST")
        .uri("/upload")
        .header("HX-Request", "true")
        .header("cookie", cookie)
        .header("content-type", format!("multipart/form-data; boundary={boundary}"))
        .body(Body::from(body))
        .expect("запит");
    let (status, _, out) = call(request).await;

    assert_eq!(status, StatusCode::OK, "{out}");
    // filename|size|content_type|is_image|text
    assert!(out.contains("нотатка.txt|"), "{out}");
    assert!(out.contains("|text/plain|false|вміст файлу"), "{out}");
}

#[tokio::test]
async fn translations_follow_the_request_locale() {
    // Типово — мова за замовчуванням із [app] locale (у фікстурі uk).
    let (status, _, body) = call(htmx("/i18n")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(r#"<p id="locale">uk</p>"#), "{body}");
    assert!(body.contains(r#"<p id="greeting">Привіт</p>"#), "{body}");
    assert!(body.contains(r#"<p id="items">У кошику 3 товарів</p>"#), "{body}");

    // `set_locale` перемикає мову в межах запиту.
    let (_, _, body) = call(htmx("/i18n?lang=en")).await;
    assert!(body.contains(r#"<p id="locale">en</p>"#), "{body}");
    assert!(body.contains(r#"<p id="greeting">Hello</p>"#), "{body}");
    assert!(body.contains(r#"<p id="items">3 items in cart</p>"#), "{body}");
    // Ключа немає в en — падаємо на мову за замовчуванням, а не на порожнечу.
    assert!(body.contains(r#"<p id="fallback">Лише українською</p>"#), "{body}");

    // Відсутній ключ показує сам себе — дірку в перекладі видно одразу.
    assert!(body.contains(r#"<p id="missing">nope.key</p>"#), "{body}");

    // Невідома мова не скидає переклад у ключі.
    let (_, _, body) = call(htmx("/i18n?lang=xx")).await;
    assert!(body.contains(r#"<p id="greeting">Привіт</p>"#), "{body}");
}
