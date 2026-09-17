//! HTTP-шар rhaix: маршрути з файлової структури, layout і правило фрагмента.
//!
//! Сервер знаходить сторінку, рендерить її шаблонізатором і віддає — з layout
//! при звичайному заході й без нього при HTMX-запиті.
//!
//! Обсяг M5: сторінки з `pages/`, фрагменти з `partials/`, спільна охорона в
//! `middleware.rhx`, база з `rhaix.toml` і міграції на старті. Кеш шаблонів і
//! watcher — у M6.

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{RawPathParams, State};
use axum::http::{header, HeaderName, HeaderValue, Request as HttpRequest, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use rhai::{Dynamic, Engine, Map, Scope};
use rhaix_db::Database;
use rhaix_script::{
    display, engine as build_engine, parse_cookies, parse_urlencoded, Deadline, Hx, Limits, Log,
    Request as ScriptRequest, RequestData, Response as ScriptResponse, ResponseData,
    State as ScriptState,
};
use rhaix_template::{Globals, Loader, Slots};
use tower_http::services::ServeDir;

/// Скільки часу дається скрипту сторінки. Далі — явна помилка, а не мовчазне
/// утримання потоку (RISKS 2.3).
const SCRIPT_BUDGET: Duration = Duration::from_secs(5);

/// Обмеження на тіло запиту.
const MAX_BODY: usize = 1024 * 1024;

/// Налаштування застосунку: `rhaix.toml` плюс те, що задав CLI.
#[derive(Debug, Clone)]
pub struct Config {
    pub root: PathBuf,
    pub addr: SocketAddr,
    /// Секція `[db]`. Якщо її немає, `db` у скрипті пояснить, чого бракує.
    pub database: Option<DatabaseConfig>,
}

/// Секція `[db]` з `rhaix.toml`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct DatabaseConfig {
    pub driver: String,
    pub url: String,
}

#[derive(Debug, Default, serde::Deserialize)]
struct ConfigFile {
    server: Option<ServerSection>,
    db: Option<DatabaseConfig>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct ServerSection {
    port: Option<u16>,
}

impl Config {
    pub fn new(root: impl Into<PathBuf>, addr: SocketAddr) -> Self {
        Self {
            root: root.into(),
            addr,
            database: None,
        }
    }

    /// Прочитати `rhaix.toml`, якщо він є.
    ///
    /// Порт із командного рядка сильніший за файл: під час розробки часто треба
    /// підняти другий сервер, не редагуючи конфіг.
    pub fn load(root: impl Into<PathBuf>, port: Option<u16>) -> anyhow::Result<Self> {
        let root = root.into();
        let path = root.join("rhaix.toml");
        let file: ConfigFile = if path.is_file() {
            let text = fs::read_to_string(&path)?;
            toml::from_str(&text).map_err(|err| anyhow::anyhow!("rhaix.toml: {err}"))?
        } else {
            ConfigFile::default()
        };

        let port = port
            .or_else(|| file.server.as_ref().and_then(|s| s.port))
            .unwrap_or(3000);

        Ok(Self {
            root,
            addr: SocketAddr::from(([127, 0, 0, 1], port)),
            database: file.db,
        })
    }

    pub fn migrations_dir(&self) -> PathBuf {
        self.root.join("migrations")
    }

    pub fn pages_dir(&self) -> PathBuf {
        self.root.join("pages")
    }

    pub fn public_dir(&self) -> PathBuf {
        self.root.join("public")
    }

    pub fn partials_dir(&self) -> PathBuf {
        self.root.join("partials")
    }

    pub fn layout_path(&self) -> PathBuf {
        self.root.join("layouts").join("main.rhx")
    }

    /// Код, що виконується перед кожним запитом (SYNTAX 6.6).
    pub fn middleware_path(&self) -> PathBuf {
        self.root.join("middleware.rhx")
    }
}

/// Що саме віддає маршрут.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteKind {
    /// Сторінка з `pages/`: при звичайному заході загортається в layout.
    Page,
    /// Фрагмент із `partials/`: layout не додається ніколи.
    Partial,
}

/// Сторінка, знайдена при скануванні `pages/`.
#[derive(Debug, Clone)]
pub struct PageRoute {
    /// Шлях у стилі axum: `/todo`, `/todo/{id}`, `/blog/{*rest}`.
    pub pattern: String,
    pub file: PathBuf,
    pub kind: RouteKind,
}

#[derive(Clone)]
struct AppState {
    config: Config,
    /// Рушій спільний для всіх запитів: `sync`-збірка Rhai дозволяє тримати
    /// його в `Arc`, а на запит створюється лише `Scope`.
    engine: Arc<Engine>,
    /// Процесне сховище `state` — одне на весь застосунок.
    state: ScriptState,
    /// Підключення до бази (або заглушка, якщо `[db]` немає).
    database: Database,
}

/// Зібрати застосунок: сторінки + статика з `public/`.
pub fn build(config: Config) -> anyhow::Result<(Router, Vec<PageRoute>)> {
    let mut routes = scan_pages(&config.pages_dir())?;
    routes.extend(scan_partials(&config.partials_dir())?);

    let database = match &config.database {
        Some(settings) => {
            // Відносний шлях у `rhaix.toml` — відносно **кореня проєкту**, а не
            // теки, з якої запустили процес. Інакше `rhaix dev ../app` створює
            // базу не там, і після деплою це виглядає як зникнення даних.
            let url = resolve_db_url(&config.root, &settings.url);
            let database =
                Database::open(&settings.driver, &url).map_err(|err| anyhow::anyhow!("{err}"))?;
            // Міграції застосовуються на старті: сервер, який піднявся, завжди
            // має схему, яку очікують сторінки.
            let applied = database
                .migrate_from(&config.migrations_dir())
                .map_err(|err| anyhow::anyhow!("{err}"))?;
            for name in &applied {
                tracing::info!("міграція застосована: {name}");
            }
            database
        }
        None => Database::unconfigured(),
    };
    let state = AppState {
        config: config.clone(),
        engine: Arc::new(build_engine(Limits::default())),
        state: ScriptState::new(),
        database,
    };

    let mut router = Router::new();
    for route in &routes {
        let file = route.file.clone();
        let kind = route.kind;
        router = router.route(
            &route.pattern,
            any(
                move |state: State<AppState>, params: RawPathParams, request: HttpRequest<Body>| {
                    let file = file.clone();
                    async move { serve_page(state.0, params, request, file, kind).await }
                },
            ),
        );
    }

    let public = config.public_dir();
    let router = if public.is_dir() {
        // `public/style.css` віддається як `/style.css` — без префікса, як в Astro.
        router.fallback_service(ServeDir::new(public).append_index_html_on_directories(false))
    } else {
        router.fallback(not_found)
    };

    Ok((router.with_state(state), routes))
}

/// Просканувати `pages/` і побудувати маршрути.
///
/// `index.rhx` → `/`, `todo.rhx` → `/todo`, `todo/[id].rhx` → `/todo/{id}`,
/// `blog/[...rest].rhx` → `/blog/{*rest}`.
pub fn scan_pages(dir: &Path) -> anyhow::Result<Vec<PageRoute>> {
    let mut routes = Vec::new();
    if !dir.is_dir() {
        return Ok(routes);
    }
    collect_pages(dir, dir, &mut routes)?;
    // Довші (специфічніші) шляхи реєструємо першими, щоб `/todo/new` не з'їдався
    // маршрутом `/todo/{id}`.
    routes.sort_by(|a, b| {
        let a_dynamic = a.pattern.contains('{');
        let b_dynamic = b.pattern.contains('{');
        a_dynamic
            .cmp(&b_dynamic)
            .then_with(|| b.pattern.len().cmp(&a.pattern.len()))
    });
    Ok(routes)
}

/// Просканувати `partials/`: `Stats.rhx` → `/components/stats`.
///
/// Це прямий аналог `/components/todo` з Node-RED-стартера, тільки без окремого
/// ендпоінта на кожен компонент — файл і є ендпоінтом.
pub fn scan_partials(dir: &Path) -> anyhow::Result<Vec<PageRoute>> {
    let mut routes = Vec::new();
    if !dir.is_dir() {
        return Ok(routes);
    }
    collect_pages(dir, dir, &mut routes)?;
    for route in &mut routes {
        route.kind = RouteKind::Partial;
        route.pattern = format!("/components{}", route.pattern.to_lowercase());
    }
    Ok(routes)
}

fn collect_pages(root: &Path, dir: &Path, out: &mut Vec<PageRoute>) -> anyhow::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_pages(root, &path, out)?;
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rhx") {
            continue;
        }
        let relative = path.strip_prefix(root).unwrap_or(&path);
        out.push(PageRoute {
            pattern: route_pattern(relative),
            file: path,
            kind: RouteKind::Page,
        });
    }
    Ok(())
}

fn route_pattern(relative: &Path) -> String {
    let mut segments: Vec<String> = Vec::new();
    for part in relative.iter() {
        let part = part.to_string_lossy();
        let name = part.strip_suffix(".rhx").unwrap_or(&part).to_string();
        segments.push(segment_pattern(&name));
    }
    if segments.last().map(|s| s == "index").unwrap_or(false) {
        segments.pop();
    }
    if segments.is_empty() {
        "/".to_owned()
    } else {
        format!("/{}", segments.join("/"))
    }
}

/// `:memory:` лишається як є, абсолютний шлях — теж; відносний стає шляхом
/// від кореня проєкту.
fn resolve_db_url(root: &Path, url: &str) -> String {
    if url == ":memory:" || url.contains("://") {
        return url.to_owned();
    }
    let path = Path::new(url);
    if path.is_absolute() {
        return url.to_owned();
    }
    root.join(path).to_string_lossy().into_owned()
}

fn segment_pattern(name: &str) -> String {
    if let Some(inner) = name.strip_prefix("[...").and_then(|s| s.strip_suffix(']')) {
        return format!("{{*{inner}}}");
    }
    if let Some(inner) = name.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return format!("{{{inner}}}");
    }
    name.to_owned()
}

async fn serve_page(
    state: AppState,
    params: RawPathParams,
    request: HttpRequest<Body>,
    file: PathBuf,
    kind: RouteKind,
) -> Response {
    let data = match collect_request(params, request).await {
        Ok(data) => data,
        Err(err) => return err.into_response(),
    };
    let is_htmx = data.is_htmx;

    // Скрипт користувача синхронний і може ходити в БД, тому виконується на
    // окремому потоці; рушій спільний (`sync`-збірка Rhai), шаблон — теж.
    let outcome = tokio::task::spawn_blocking(move || render_page(&state, &file, kind, data)).await;

    let (body, response_state) = match outcome {
        Ok(Ok(result)) => result,
        Ok(Err(err)) => return err.into_response(),
        Err(err) => {
            tracing::error!("рендер не завершився: {err}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "500").into_response();
        }
    };

    build_response(body, response_state, is_htmx)
}

/// Зібрати дані запиту у вигляді, зрозумілому скрипту.
async fn collect_request(
    params: RawPathParams,
    request: HttpRequest<Body>,
) -> Result<RequestData, PageError> {
    let (parts, body) = request.into_parts();

    let mut headers = std::collections::BTreeMap::new();
    for (name, value) in parts.headers.iter() {
        if let Ok(text) = value.to_str() {
            headers.insert(name.as_str().to_ascii_lowercase(), text.to_owned());
        }
    }
    let cookies = headers
        .get("cookie")
        .map(|raw| parse_cookies(raw))
        .unwrap_or_default();

    let bytes = axum::body::to_bytes(body, MAX_BODY)
        .await
        .map_err(|err| PageError::Io {
            file: PathBuf::from("<body>"),
            message: format!("не вдалося прочитати тіло запиту: {err}"),
        })?;
    let body_text = String::from_utf8_lossy(&bytes).into_owned();

    let is_form = headers
        .get("content-type")
        .map(|value| value.starts_with("application/x-www-form-urlencoded"))
        .unwrap_or(false);

    Ok(RequestData {
        method: parts.method.as_str().to_owned(),
        path: parts.uri.path().to_owned(),
        params: params
            .iter()
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect(),
        query: parts.uri.query().map(parse_urlencoded).unwrap_or_default(),
        form: if is_form {
            parse_urlencoded(&body_text)
        } else {
            Default::default()
        },
        is_htmx: headers.contains_key("hx-request"),
        headers,
        cookies,
        body: body_text,
    })
}

/// Скласти HTTP-відповідь із того, що попросив скрипт.
fn build_response(body: String, state: ResponseData, is_htmx: bool) -> Response {
    let mut status = StatusCode::from_u16(state.status).unwrap_or(StatusCode::OK);
    let mut response = Response::new(Body::from(body));

    {
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        // Vary самого по собі мало: деякі CDN його ігнорують і можуть віддати
        // фрагмент замість сторінки (RISKS 2.9), тому HTMX-відповіді не кешуємо.
        headers.insert(header::VARY, HeaderValue::from_static("HX-Request"));
        if is_htmx {
            headers.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, no-store"),
            );
        }

        for (name, value) in &state.headers {
            insert_header(headers, name, value);
        }
        for cookie in &state.cookies {
            if let Ok(value) = HeaderValue::from_str(cookie) {
                headers.append(header::SET_COOKIE, value);
            }
        }
        if let Some(trigger) = rhaix_script::triggers_header(&state.triggers) {
            insert_header(headers, "HX-Trigger", &trigger);
            insert_header(headers, "Access-Control-Expose-Headers", "HX-Trigger");
        }
        if state.refresh {
            insert_header(headers, "HX-Refresh", "true");
        }
        if let Some(url) = &state.redirect {
            // htmx сам не піде за 302 у фрагменті — для нього потрібен заголовок,
            // а для звичайного заходу — звичайний редірект.
            if is_htmx {
                insert_header(headers, "HX-Redirect", url);
            } else {
                insert_header(headers, "Location", url);
                status = StatusCode::SEE_OTHER;
            }
        }
    }

    *response.status_mut() = status;
    response
}

fn insert_header(headers: &mut axum::http::HeaderMap, name: &str, value: &str) {
    match (
        HeaderName::from_bytes(name.as_bytes()),
        HeaderValue::from_str(value),
    ) {
        (Ok(name), Ok(value)) => {
            headers.insert(name, value);
        }
        // Найчастіша причина — не-ASCII у значенні. Мовчки загубити заголовок
        // гірше, ніж сказати про це: користувач шукатиме зниклий тост годинами.
        _ => tracing::warn!("заголовок `{name}` не додано: значення має бути ASCII"),
    }
}

/// Виконати логіку сторінки, відрендерити її і, якщо треба, вкласти в layout.
///
/// Правило фрагмента (SYNTAX 6.3): layout додається лише тоді, коли сторінку
/// відкривають напряму. Для HTMX-запиту віддається сама розмітка сторінки.
fn render_page(
    state: &AppState,
    file: &Path,
    kind: RouteKind,
    data: RequestData,
) -> Result<(String, ResponseData), PageError> {
    let _deadline = Deadline::new(SCRIPT_BUDGET);

    let is_htmx = data.is_htmx;
    let path = data.path.clone();
    let response = ScriptResponse::new();

    // Один завантажувач на запит: він компілює сторінку разом з усіма її
    // компонентами й кешує їх на час цього рендеру (постійний кеш — M6).
    let loader = Loader::new(state.config.root.clone(), state.engine.clone());
    let template = loader
        .load(file)
        .map_err(|diagnostic| PageError::Template {
            diagnostic: diagnostic.text(),
        })?;

    // `page` — спільне значення: компонент і layout бачать те саме, що й сторінка.
    let page = Dynamic::from_map(Map::new()).into_shared();
    let globals = globals_for(
        &response,
        &state.state,
        &state.database,
        data,
        display_path(&state.config.root, file),
        page,
    );

    // `middleware.rhx` виконується перед сторінкою: автентифікація, права,
    // локаль — усе, що інакше довелось би дублювати в кожному файлі (SYNTAX 6.6).
    let middleware_path = state.config.middleware_path();
    if middleware_path.is_file() {
        let middleware =
            loader
                .load(&middleware_path)
                .map_err(|diagnostic| PageError::Template {
                    diagnostic: diagnostic.text(),
                })?;
        let mut middleware_scope = Scope::new();
        globals.apply(&mut middleware_scope);
        let returned = middleware
            .run_script(&state.engine, &mut middleware_scope)
            .map_err(|diagnostic| PageError::Template {
                diagnostic: diagnostic.text(),
            })?;

        let state_now = response.take();
        if state_now.stop {
            return Ok((String::new(), state_now));
        }
        if !returned.is_unit() {
            // middleware віддав готове тіло — сторінка не виконується взагалі
            return Ok((display(&returned), state_now));
        }
    }

    let mut scope = Scope::new();
    globals.apply(&mut scope);
    let returned = template
        .run_script(&state.engine, &mut scope)
        .map_err(|diagnostic| PageError::Template {
            diagnostic: diagnostic.text(),
        })?;

    let mut state_now = response.take();
    // `res.redirect(...)`, `res.status(404)`, `hx.refresh()` — розмітку не рендеримо.
    if state_now.stop {
        return Ok((String::new(), state_now));
    }

    // Значення, повернуте скриптом, стає тілом як є: `return "";` прибирає
    // елемент, `return raw(...)` віддає готовий HTML.
    let page_html = if !returned.is_unit() {
        display(&returned)
    } else {
        let rendered = template
            .render(&state.engine, &mut scope, Slots::default(), &globals)
            .map_err(|diagnostic| PageError::Template {
                diagnostic: diagnostic.text(),
            })?;
        for warning in &rendered.warnings {
            tracing::warn!("{path}: {warning}");
        }
        rendered.html
    };

    // Фрагмент із `partials/` не загортається в layout ніколи: він для того й
    // існує, щоб приїхати в уже відкриту сторінку.
    if is_htmx || kind == RouteKind::Partial {
        state_now = response.take();
        return Ok((page_html, state_now));
    }

    let layout_path = state.config.layout_path();
    if !layout_path.is_file() {
        state_now = response.take();
        return Ok((page_html, state_now));
    }

    // `page` (title та інше) переїжджає зі сторінки в layout сам собою: це те
    // саме спільне значення. Саме тому layout рендериться після сторінки.
    let layout = loader
        .load(&layout_path)
        .map_err(|diagnostic| PageError::Template {
            diagnostic: diagnostic.text(),
        })?;
    let mut layout_scope = Scope::new();
    globals.apply(&mut layout_scope);
    let _ = layout
        .run_script(&state.engine, &mut layout_scope)
        .map_err(|diagnostic| PageError::Template {
            diagnostic: diagnostic.text(),
        })?;

    let head = collect_head(&state.config.public_dir());
    let scripts = collect_scripts(&state.config.public_dir());
    let wrapped = layout
        .render(
            &state.engine,
            &mut layout_scope,
            Slots {
                slot: &page_html,
                head: &head,
                scripts: &scripts,
            },
            &globals,
        )
        .map_err(|diagnostic| PageError::Template {
            diagnostic: diagnostic.text(),
        })?;
    for warning in &wrapped.warnings {
        tracing::warn!("{path}: {warning}");
    }

    state_now = response.take();
    Ok((wrapped.html, state_now))
}

/// Усі `public/**.css` — у `<head>`.
fn collect_head(public: &Path) -> String {
    list_assets(public, "css")
        .into_iter()
        .map(|href| format!("<link rel=\"stylesheet\" href=\"{href}\">"))
        .collect::<Vec<_>>()
        .join(
            "
  ",
        )
}

/// htmx першим, далі решта `public/**.js` — саме те, що в Node-RED-стартері
/// доводилось вписувати в `index.html` руками.
fn collect_scripts(public: &Path) -> String {
    let mut scripts = list_assets(public, "js");
    scripts.sort_by_key(|src| !src.contains("htmx"));
    scripts
        .into_iter()
        .map(|src| format!("<script src=\"{src}\"></script>"))
        .collect::<Vec<_>>()
        .join(
            "
  ",
        )
}

fn list_assets(public: &Path, extension: &str) -> Vec<String> {
    fn walk(dir: &Path, base: &Path, extension: &str, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, base, extension, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some(extension) {
                if let Ok(rel) = path.strip_prefix(base) {
                    out.push(format!("/{}", rel.to_string_lossy().replace('\\', "/")));
                }
            }
        }
    }

    let mut found = Vec::new();
    walk(public, public, extension, &mut found);
    found.sort();
    found
}

async fn not_found(_: HttpRequest<Body>) -> Response {
    (StatusCode::NOT_FOUND, "404").into_response()
}

/// Об'єкти, які бачить кожен файл рендеру — і сторінка, і layout, і компоненти.
///
/// Компонент не успадковує scope батька (SYNTAX 5.3), тому глобальні об'єкти
/// передаються окремо — інакше в компоненті не було б ні `req`, ні `page`.
#[allow(clippy::too_many_arguments)]
fn globals_for(
    response: &ScriptResponse,
    state: &ScriptState,
    database: &Database,
    data: RequestData,
    source: String,
    page: Dynamic,
) -> Globals {
    let mut globals = Globals::new();
    globals
        .set("req", Dynamic::from(ScriptRequest::new(data)))
        .set("res", Dynamic::from(response.clone()))
        .set("hx", Dynamic::from(Hx::new(response.clone())))
        .set("state", Dynamic::from(state.clone()))
        .set("db", Dynamic::from(database.clone()))
        .set("log", Dynamic::from(Log { source }))
        .set("page", page);
    globals
}

enum PageError {
    Io { file: PathBuf, message: String },
    Template { diagnostic: String },
}

impl IntoResponse for PageError {
    fn into_response(self) -> Response {
        // Прототип dev-overlay: людина має бачити свій файл і свій рядок, а не
        // стек Rust. У M6 це піде ще й у браузер через SSE.
        let (status, body) = match self {
            PageError::Io { file, message } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "error: не вдалося прочитати {}\n  = {message}",
                    file.display()
                ),
            ),
            PageError::Template { diagnostic } => (StatusCode::INTERNAL_SERVER_ERROR, diagnostic),
        };
        tracing::error!("{body}");
        let html = format!(
            "<!DOCTYPE html><meta charset=\"utf-8\"><title>rhaix — помилка</title>\
             <body style=\"font:14px/1.5 ui-monospace,monospace;padding:2rem;background:#111;color:#eee\">\
             <pre>{}</pre></body>",
            html_escape(&body)
        );
        (
            status,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            html,
        )
            .into_response()
    }
}

/// Шлях для показу людині: відносний до кореня, з прямими слешами.
fn display_path(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Запустити сервер і працювати до Ctrl+C.
pub async fn serve(config: Config) -> anyhow::Result<()> {
    let (router, routes) = build(config.clone())?;

    println!("rhaix dev — http://{}", config.addr);
    println!("  корінь : {}", config.root.display());
    for route in &routes {
        let mark = match route.kind {
            RouteKind::Page => "сторінка",
            RouteKind::Partial => "фрагмент",
        };
        println!(
            "  {mark}: {:<22} {}",
            route.pattern,
            display_path(&config.root, &route.file)
        );
    }
    if config.middleware_path().is_file() {
        println!("  middleware: middleware.rhx — виконується перед кожним запитом");
    }
    match &config.database {
        Some(settings) => println!("  база   : {} — {}", settings.driver, settings.url),
        None => println!("  база   : не налаштована (секція [db] у rhaix.toml)"),
    }
    println!("  M5: дані, маршрути й компоненти працюють; кеш і watcher — з M6\n");

    let listener = tokio::net::TcpListener::bind(config.addr).await?;
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_files_to_routes() {
        assert_eq!(route_pattern(Path::new("index.rhx")), "/");
        assert_eq!(route_pattern(Path::new("todo.rhx")), "/todo");
        assert_eq!(route_pattern(Path::new("blog/index.rhx")), "/blog");
        assert_eq!(route_pattern(Path::new("todo/[id].rhx")), "/todo/{id}");
        assert_eq!(
            route_pattern(Path::new("blog/[...rest].rhx")),
            "/blog/{*rest}"
        );
        assert_eq!(route_pattern(Path::new("api/todo.rhx")), "/api/todo");
    }

    #[test]
    fn static_routes_are_registered_before_dynamic_ones() {
        let mut routes = [
            PageRoute {
                pattern: "/todo/{id}".into(),
                file: PathBuf::new(),
                kind: RouteKind::Page,
            },
            PageRoute {
                pattern: "/todo/new".into(),
                file: PathBuf::new(),
                kind: RouteKind::Page,
            },
        ];
        routes.sort_by(|a, b| {
            let a_dynamic = a.pattern.contains('{');
            let b_dynamic = b.pattern.contains('{');
            a_dynamic
                .cmp(&b_dynamic)
                .then_with(|| b.pattern.len().cmp(&a.pattern.len()))
        });
        assert_eq!(routes[0].pattern, "/todo/new");
    }

    #[test]
    fn diagnostic_paths_are_relative_to_the_project_root() {
        let root = Path::new("C:/proj");
        let file = Path::new("C:/proj/pages/todo.rhx");
        assert_eq!(display_path(root, file), "pages/todo.rhx");
    }

    #[test]
    fn relative_database_paths_are_rooted_at_the_project() {
        let resolved = resolve_db_url(Path::new("C:/app"), "data/app.db");
        assert!(
            resolved.replace('\\', "/").ends_with("C:/app/data/app.db"),
            "{resolved}"
        );
        assert_eq!(resolve_db_url(Path::new("C:/app"), ":memory:"), ":memory:");
        assert_eq!(
            resolve_db_url(Path::new("C:/app"), "postgres://localhost/db"),
            "postgres://localhost/db"
        );
    }

    #[test]
    fn scripts_put_htmx_first() {
        let mut scripts = ["/app.js".to_owned(), "/vendor/htmx.min.js".to_owned()];
        scripts.sort_by_key(|src| !src.contains("htmx"));
        assert_eq!(scripts[0], "/vendor/htmx.min.js");
    }
}
