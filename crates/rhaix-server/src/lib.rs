//! HTTP-шар rhaix: маршрути з файлової структури, layout і правило фрагмента.
//!
//! Сервер знаходить сторінку, рендерить її шаблонізатором і віддає — з layout
//! при звичайному заході й без нього при HTMX-запиті.
//!
//! Два режими: розробка (живе перезавантаження, свіжість файлів на кожен запит)
//! і продакшн (заморожений кеш, стиснення). Файли беруться через трейт `Files`,
//! тому той самий сервер працює і з диска, і з таблиці, вшитої в бінарник.

mod check;
mod client;

pub use check::{check, Issue};
pub use client::{CLIENT_JS, CLIENT_ROUTE};

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{RawPathParams, State};
use axum::http::{header, HeaderName, HeaderValue, Request as HttpRequest, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
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
use rhaix_template::{DiskFiles, Files, Globals, Loader, Slots, TemplateCache};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

/// Скільки часу дається скрипту сторінки. Далі — явна помилка, а не мовчазне
/// утримання потоку (RISKS 2.3).
const SCRIPT_BUDGET: Duration = Duration::from_secs(5);

/// Обмеження на тіло запиту.
const MAX_BODY: usize = 1024 * 1024;

/// Скільки чекати після події файлової системи, перш ніж перезбирати.
///
/// Редактори пишуть файл кількома операціями, тож без паузи одне збереження
/// давало б два-три перезавантаження сторінки.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(50);

/// Канал, яким `rhaix dev` повідомляє браузеру, що пора перезавантажитись.
const RELOAD_ROUTE: &str = "/_rhaix/events";

/// Налаштування застосунку: `rhaix.toml` плюс те, що задав CLI.
#[derive(Clone)]
pub struct Config {
    pub root: PathBuf,
    pub addr: SocketAddr,
    /// Секція `[db]`. Якщо її немає, `db` у скрипті пояснить, чого бракує.
    pub database: Option<DatabaseConfig>,
    /// Режим розробки: живе перезавантаження й перевірка свіжості файлів.
    pub dev: bool,
    /// Звідки читати файли проєкту: диск або вшита в бінарник таблиця.
    pub files: Arc<dyn Files>,
    /// Чи застосунок вшитий у бінарник (тоді статика теж іде з таблиці).
    pub embedded: bool,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("root", &self.root)
            .field("addr", &self.addr)
            .field("database", &self.database)
            .field("dev", &self.dev)
            .finish()
    }
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
            dev: true,
            files: DiskFiles::shared(),
            embedded: false,
        }
    }

    /// Конфіг для зібраного бінарника: файли беруться з вшитої таблиці,
    /// режим — продакшн, коренем є порожній шлях.
    ///
    /// Цим користується код, який генерує `rhaix build`.
    pub fn embedded(files: Arc<dyn Files>, port: Option<u16>) -> anyhow::Result<Self> {
        let raw = files.read_text(Path::new("rhaix.toml")).unwrap_or_default();
        let file: ConfigFile =
            toml::from_str(&raw).map_err(|err| anyhow::anyhow!("rhaix.toml: {err}"))?;
        let port = port
            .or_else(|| file.server.as_ref().and_then(|s| s.port))
            .unwrap_or(3000);

        Ok(Self {
            root: PathBuf::new(),
            addr: SocketAddr::from(([0, 0, 0, 0], port)),
            database: file.db,
            dev: false,
            files,
            embedded: true,
        })
    }

    /// Те саме, але для продакшну: без стеження за файлами й без клієнта
    /// живого перезавантаження.
    pub fn load_release(root: impl Into<PathBuf>, port: Option<u16>) -> anyhow::Result<Self> {
        let mut config = Self::load(root, port)?;
        config.dev = false;
        Ok(config)
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
            dev: true,
            files: DiskFiles::shared(),
            embedded: false,
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
    /// Кеш скомпільованих шаблонів, спільний для всіх запитів.
    templates: Arc<TemplateCache>,
    /// Сповіщення про зміну файлів для відкритих сторінок.
    reload: broadcast::Sender<()>,
}

/// Зібрати застосунок: сторінки + статика з `public/`.
pub fn build(config: Config) -> anyhow::Result<(Router, Vec<PageRoute>)> {
    build_watched(config).map(|(router, routes, _)| (router, routes))
}

/// Те саме, але ще й віддає кеш і канал перезавантаження — щоб `serve`
/// міг повісити на них watcher.
#[allow(clippy::type_complexity)]
pub fn build_watched(
    config: Config,
) -> anyhow::Result<(
    Router,
    Vec<PageRoute>,
    (Arc<TemplateCache>, broadcast::Sender<()>),
)> {
    let mut routes = scan_pages(config.files.as_ref(), &config.pages_dir())?;
    routes.extend(scan_partials(
        config.files.as_ref(),
        &config.partials_dir(),
    )?);

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
            let migrations: Vec<(String, String)> = config
                .files
                .list(&config.migrations_dir(), "sql")
                .into_iter()
                .filter_map(|path| {
                    let name = path.file_name()?.to_string_lossy().into_owned();
                    let body = config.files.read_text(&path)?;
                    Some((name, body))
                })
                .collect();
            let applied = database
                .migrate(&migrations)
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
        // У dev кеш перевіряє свіжість файлів на кожен запит; у продакшні
        // шаблон компілюється один раз і більше ніколи не читається з диска.
        templates: if config.dev {
            TemplateCache::watching()
        } else {
            TemplateCache::frozen()
        },
        reload: broadcast::channel(16).0,
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

    // Канал живого перезавантаження. У проді маршрут просто не потрібен, але
    // тримати його окремо від сторінок усе одно правильно: це службовий шлях.
    let reload = state.reload.clone();
    let router = router.route(
        RELOAD_ROUTE,
        axum::routing::get(move || {
            let stream = BroadcastStream::new(reload.subscribe())
                .map(|_| Ok::<Event, std::convert::Infallible>(Event::default().event("reload")));
            std::future::ready(Sse::new(stream).keep_alive(KeepAlive::default()))
        }),
    );

    let router = router.route(
        CLIENT_ROUTE,
        axum::routing::get(|| async {
            (
                [
                    (
                        header::CONTENT_TYPE,
                        "application/javascript; charset=utf-8",
                    ),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
                CLIENT_JS,
            )
        }),
    );

    if config.embedded {
        // Вшита статика: ServeDir тут ні до чого — файлів на диску немає.
        let files = config.files.clone();
        let public = config.public_dir();
        let router = router.fallback(move |uri: axum::http::Uri| {
            let files = files.clone();
            let public = public.clone();
            async move { serve_embedded_asset(files.as_ref(), &public, uri.path()) }
        });
        let router = router.layer(CompressionLayer::new());
        let watched = (state.templates.clone(), state.reload.clone());
        return Ok((router.with_state(state), routes, watched));
    }

    let public = config.public_dir();
    let router = if public.is_dir() {
        // `public/style.css` віддається як `/style.css` — без префікса, як в Astro.
        let files = ServeDir::new(public).append_index_html_on_directories(false);
        if config.dev {
            router.fallback_service(files)
        } else {
            // У продакшні статика кешується браузером: вона змінюється лише
            // разом із деплоєм.
            router.fallback_service(
                ServiceBuilder::new()
                    .layer(SetResponseHeaderLayer::if_not_present(
                        header::CACHE_CONTROL,
                        HeaderValue::from_static("public, max-age=3600"),
                    ))
                    .service(files),
            )
        }
    } else {
        router.fallback(not_found)
    };

    // Стиснення вмикається лише в продакшні: у розробці воно тільки заважає
    // дивитись відповіді очима.
    let router = if config.dev {
        router
    } else {
        router.layer(CompressionLayer::new())
    };

    let watched = (state.templates.clone(), state.reload.clone());
    Ok((router.with_state(state), routes, watched))
}

/// Просканувати `pages/` і побудувати маршрути.
///
/// `index.rhx` → `/`, `todo.rhx` → `/todo`, `todo/[id].rhx` → `/todo/{id}`,
/// `blog/[...rest].rhx` → `/blog/{*rest}`.
pub fn scan_pages(files: &dyn Files, dir: &Path) -> anyhow::Result<Vec<PageRoute>> {
    let mut routes = Vec::new();
    for path in files.list(dir, "rhx") {
        let relative = path.strip_prefix(dir).unwrap_or(&path).to_path_buf();
        routes.push(PageRoute {
            pattern: route_pattern(&relative),
            file: path,
            kind: RouteKind::Page,
        });
    }
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
pub fn scan_partials(files: &dyn Files, dir: &Path) -> anyhow::Result<Vec<PageRoute>> {
    let mut routes = scan_pages(files, dir)?;
    for route in &mut routes {
        route.kind = RouteKind::Partial;
        route.pattern = format!("/components{}", route.pattern.to_lowercase());
    }
    Ok(routes)
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

    // Завантажувач створюється на запит, але кеш у нього спільний: повторний
    // запит бере готове дерево, а правка файлу робить запис несвіжим.
    let loader = Loader::with_files(
        state.config.root.clone(),
        state.engine.clone(),
        state.templates.clone(),
        state.config.files.clone(),
    );
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
    if state.config.files.exists(&middleware_path) {
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
    let mut assets = Assets::default();
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
        assets = Assets::from(&rendered);
        rendered.html
    };

    // Фрагмент із `partials/` не загортається в layout ніколи: він для того й
    // існує, щоб приїхати в уже відкриту сторінку.
    if is_htmx || kind == RouteKind::Partial {
        state_now = response.take();
        // Асети їдуть разом із фрагментом: стиль позначений хешем, скрипт
        // загорнутий у перевірку реєстру, тож повторно не виконається.
        return Ok((assets.append_to(page_html), state_now));
    }

    let layout_path = state.config.layout_path();
    if !state.config.files.exists(&layout_path) {
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

    // Підняті стилі — у `<rhaix:head/>`, підняті скрипти — у `<rhaix:scripts/>`.
    let head = format!(
        "{}{}",
        collect_head(state.config.files.as_ref(), &state.config.public_dir()),
        assets.head()
    );
    let scripts = format!("{}{}", collect_scripts(&state.config), assets.scripts());
    let wrapped = layout
        .render_with(
            &state.engine,
            &mut layout_scope,
            Slots {
                slot: &page_html,
                head: &head,
                scripts: &scripts,
            },
            &globals,
            // layout сам є документом: його власні теги лишаються на місці
            false,
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
fn collect_head(files: &dyn Files, public: &Path) -> String {
    list_assets(files, public, "css")
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
fn collect_scripts(config: &Config) -> String {
    let mut scripts = list_assets(config.files.as_ref(), &config.public_dir(), "js");
    scripts.sort_by_key(|src| !src.contains("htmx"));

    // `rhaix.js` іде перший: підняті скрипти компонентів питають у нього
    // реєстр, тому він має бути вже завантажений.
    let mut out: Vec<String> = vec![format!("<script src=\"{CLIENT_ROUTE}\"></script>")];
    out.extend(
        scripts
            .into_iter()
            .map(|src| format!("<script src=\"{src}\"></script>")),
    );

    // У режимі розробки додається крихітний клієнт живого перезавантаження:
    // сторінка оновлюється сама, щойно watcher побачив зміну.
    if config.dev {
        out.push(format!(
            "<script>new EventSource(\"{RELOAD_ROUTE}\").addEventListener(\"reload\", () => location.reload());</script>"
        ));
    }
    out.join("\n  ")
}

fn list_assets(files: &dyn Files, public: &Path, extension: &str) -> Vec<String> {
    let mut found: Vec<String> = files
        .list(public, extension)
        .into_iter()
        .filter_map(|path| {
            let relative = path.strip_prefix(public).ok()?;
            Some(format!(
                "/{}",
                relative.to_string_lossy().replace('\\', "/")
            ))
        })
        .collect();
    found.sort();
    found
}

/// Віддати вшитий файл із `public/`.
fn serve_embedded_asset(files: &dyn Files, public: &Path, path: &str) -> Response {
    let relative = path.trim_start_matches('/');
    if relative.is_empty() || relative.contains("..") {
        return (StatusCode::NOT_FOUND, "404").into_response();
    }
    let Some(bytes) = files.read(&public.join(relative)) else {
        return (StatusCode::NOT_FOUND, "404").into_response();
    };

    let mime = match Path::new(relative).extension().and_then(|e| e.to_str()) {
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("json") => "application/json",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    };
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        bytes,
    )
        .into_response()
}

async fn not_found(_: HttpRequest<Body>) -> Response {
    (StatusCode::NOT_FOUND, "404").into_response()
}

/// Підняті зі сторінки й компонентів `<style>`/`<script>`.
#[derive(Debug, Default)]
struct Assets {
    styles: Vec<rhaix_template::Asset>,
    scripts: Vec<rhaix_template::Asset>,
}

impl Assets {
    fn from(rendered: &rhaix_template::Rendered) -> Self {
        Self {
            styles: rendered.styles.clone(),
            scripts: rendered.scripts.clone(),
        }
    }

    fn head(&self) -> String {
        self.styles
            .iter()
            .map(|asset| client::style_tag(&asset.hash, &asset.body))
            .collect::<Vec<_>>()
            .join("\n  ")
    }

    fn scripts(&self) -> String {
        self.scripts
            .iter()
            .map(|asset| client::script_tag(&asset.hash, &asset.body))
            .collect::<Vec<_>>()
            .join("\n  ")
    }

    /// Для фрагмента асети додаються в кінець — іншого місця немає.
    fn append_to(&self, mut html: String) -> String {
        for asset in &self.styles {
            html.push_str(&client::style_tag(&asset.hash, &asset.body));
        }
        for asset in &self.scripts {
            html.push_str(&client::script_tag(&asset.hash, &asset.body));
        }
        html
    }
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
pub(crate) fn display_path(root: &Path, file: &Path) -> String {
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

/// Стежити за файлами проєкту: чистити кеш і будити відкриті сторінки.
///
/// Watcher живе у власному потоці `notify` і зупиняється разом із процесом.
fn watch(root: PathBuf, templates: Arc<TemplateCache>, reload: broadcast::Sender<()>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = match notify::recommended_watcher(tx) {
        Ok(watcher) => watcher,
        Err(err) => {
            tracing::warn!("стеження за файлами недоступне: {err}");
            return;
        }
    };
    if let Err(err) = notify::Watcher::watch(&mut watcher, &root, notify::RecursiveMode::Recursive)
    {
        tracing::warn!("стеження за файлами недоступне: {err}");
        return;
    }

    std::thread::spawn(move || {
        // watcher має жити стільки ж, скільки цикл: інакше події просто
        // перестануть приходити
        let _watcher = watcher;
        while let Ok(event) = rx.recv() {
            let Ok(event) = event else { continue };
            if !event.paths.iter().any(|path| is_source(path)) {
                continue;
            }
            // з'їдаємо решту пачки, щоб одне збереження не дало кількох перезборів
            std::thread::sleep(WATCH_DEBOUNCE);
            while rx.try_recv().is_ok() {}

            templates.clear();
            let _ = reload.send(());
            tracing::info!("зміни підхоплено");
        }
    });
}

/// Чи варто реагувати на цей файл.
fn is_source(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rhx" | "rhai" | "css" | "js" | "toml" | "sql") => true,
        // тимчасові файли редакторів ігноруємо
        _ => false,
    }
}

/// Запустити сервер і працювати до Ctrl+C.
pub async fn serve(config: Config) -> anyhow::Result<()> {
    let (router, routes, watched) = build_watched(config.clone())?;
    if config.dev {
        watch(config.root.clone(), watched.0, watched.1);
    }

    println!("rhaix — http://{}", config.addr);
    if config.embedded {
        println!("  джерело: файли вшиті в бінарник");
    } else {
        println!("  корінь : {}", config.root.display());
    }
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
    if config.files.exists(&config.middleware_path()) {
        println!("  middleware: middleware.rhx — виконується перед кожним запитом");
    }
    match &config.database {
        Some(settings) => println!("  база   : {} — {}", settings.driver, settings.url),
        None => println!("  база   : не налаштована (секція [db] у rhaix.toml)"),
    }
    if config.dev {
        println!("  режим  : розробка — живе перезавантаження увімкнено\n");
    } else {
        println!("  режим  : продакшн — кеш заморожено, стиснення увімкнено\n");
    }

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
