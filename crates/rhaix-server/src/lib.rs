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
mod config_keys;
mod lint;
mod multipart;
mod openapi;
mod scripts;

pub use check::{check, Issue, Severity};
pub use client::{CLIENT_JS, CLIENT_ROUTE, HTMX_JS, HTMX_ROUTE, UI_JS, UI_OVERRIDE, UI_ROUTE};
pub use openapi::openapi;

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{ConnectInfo, RawPathParams, State};
use axum::http::{header, HeaderName, HeaderValue, Request as HttpRequest, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use rhai::{Dynamic, Engine, Map, Scope};
use rhaix_db::Database;
use rhaix_script::{
    display, engine as build_engine, parse_cookies, parse_tz_offset, parse_urlencoded, Catalog,
    Csrf, Deadline, Http, Hx, I18n, Limits, Live, LocaleScope, Log, Mail, MailConfig,
    Request as ScriptRequest, RequestData, Response as ScriptResponse, ResponseData, Secret,
    Session, SessionOptions, State as ScriptState, UploadData, CSRF_FIELD, CSRF_HEADER,
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
/// Максимум тіла запиту. Форми крихітні, але сюди ж іде multipart із файлами,
/// тому межа щедріша. У проді її варто виносити в конфіг.
const MAX_BODY: usize = 16 * 1024 * 1024;

/// Скільки чекати після події файлової системи, перш ніж перезбирати.
///
/// Редактори пишуть файл кількома операціями, тож без паузи одне збереження
/// давало б два-три перезавантаження сторінки.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(50);

/// Канал, яким `rhaix dev` повідомляє браузеру, що пора перезавантажитись.
const RELOAD_ROUTE: &str = "/_rhaix/events";
/// Канал живих оновлень (`live.send`): `/_rhaix/live?topics=orders,users`.
pub const LIVE_ROUTE: &str = "/_rhaix/live";
/// Скільки повідомлень може накопичитись для повільного підписника, перш
/// ніж він їх пропустить. Пропуск не страшний: клієнт на перепідключенні
/// перезапитує все, що слухає.
const LIVE_BUFFER: usize = 1024;

/// Одне повідомлення каналу: тема й detail у JSON.
#[derive(Debug)]
struct LiveMessage {
    topic: String,
    detail: String,
}

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
    /// Секція `[app]`: секрет, сесія, CSRF, часовий пояс.
    pub app: AppConfig,
    /// Власні функції на Rust (`native/lib.rs` або `with_native`).
    pub native: Native,
}

/// Rhai, на якому працюють скрипти, — для коду проєкту на Rust:
/// `rhaix_server::rhai::Engine`. Власна залежність `rhai` в іншій версії дала
/// б інший тип `Engine`, і `register` просто не зібрався б.
pub use rhai;

/// Функція, що реєструє власні Rust-функції в рушії скриптів.
///
/// Коли Rhai не вистачає — важка математика, чужий крейт, швидкий парсер —
/// проєкт пише звичайну функцію на Rust і реєструє її тут. Для скрипта вона
/// нічим не відрізняється від вбудованих: `vat(total)`, `qr_svg(url)`.
#[derive(Clone, Default)]
pub struct Native(Option<Register>);

/// `fn register(engine: &mut Engine)` проєкту.
type Register = Arc<dyn Fn(&mut Engine) + Send + Sync>;

impl std::fmt::Debug for Native {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "Native(так)"
        } else {
            "Native(ні)"
        })
    }
}

impl Config {
    /// Додати власні функції на Rust.
    ///
    /// ```ignore
    /// let config = rhaix_server::Config::load(".", None)?
    ///     .with_native(|engine| {
    ///         engine.register_fn("vat", |amount: f64| amount * 0.2);
    ///     });
    /// rhaix_server::serve(config).await
    /// ```
    ///
    /// `rhaix build`, `rhaix dev` і `rhaix serve` роблять це самі, якщо в
    /// проєкті є `native/lib.rs` із `pub fn register(engine: &mut Engine)`.
    pub fn with_native(mut self, register: impl Fn(&mut Engine) + Send + Sync + 'static) -> Self {
        self.native = Native(Some(Arc::new(register)));
        self
    }
}

/// Те, що налаштовує поведінку застосунку, а не сервера.
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// Ключ підпису cookie. Звідки він береться — див. [`resolve_secret`].
    pub secret: Secret,
    /// Перевіряти CSRF-токен у запитах, що змінюють дані. Увімкнено завжди,
    /// крім явного `csrf = false`.
    pub csrf: bool,
    pub session: SessionOptions,
    /// Зсув показу дат від UTC у хвилинах.
    pub tz_offset: i32,
    /// Скільки `http` чекає на чужий сервер.
    pub http_timeout: Duration,
    /// Секція `[mail]`: SMTP або dev-лог.
    pub mail: MailConfig,
    /// Мова за замовчуванням (`[app] locale`).
    pub locale: String,
    /// `[server] trust_proxy`: застосунок стоїть за своїм зворотним проксі
    /// (nginx, Caddy), і `req.ip` береться з `X-Forwarded-For`.
    pub trust_proxy: bool,
    /// `[api] cors`: яким сайтам браузер дозволить читати відповіді `api/`.
    pub cors: Cors,
    /// `[api] title` / `version` — для опису OpenAPI.
    pub api_title: String,
    pub api_version: String,
    /// `[api] openapi = true` — віддавати опис на `/api/openapi.json`.
    pub api_docs: bool,
}

/// Хто може звертатись до `api/` з браузера на іншому сайті.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Cors {
    /// За замовчуванням: лише той самий сайт, як і без CORS узагалі.
    #[default]
    Off,
    /// `cors = "*"` — будь-який сайт. У `api/` немає cookie-сесії, тож чужий
    /// сайт не може діяти від імені залогіненого користувача: він побачить
    /// рівно те, що побачив би `curl` без токена.
    Any,
    /// `cors = ["https://app.example.com"]` — лише ці адреси.
    List(Vec<String>),
}

impl Cors {
    /// Значення `Access-Control-Allow-Origin` для цього `Origin`, якщо можна.
    fn allow(&self, origin: Option<&str>) -> Option<String> {
        let origin = origin?;
        match self {
            Cors::Off => None,
            Cors::Any => Some("*".to_owned()),
            Cors::List(list) => list
                .iter()
                .any(|allowed| allowed.trim_end_matches('/').eq_ignore_ascii_case(origin))
                .then(|| origin.to_owned()),
        }
    }
}

/// `[api] cors` у файлі: рядок `"*"` або список адрес.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(untagged)]
enum CorsSetting {
    One(String),
    Many(Vec<String>),
}

impl CorsSetting {
    fn into_cors(self) -> Cors {
        match self {
            CorsSetting::One(value) if value.trim() == "*" => Cors::Any,
            CorsSetting::One(value) if value.trim().is_empty() => Cors::Off,
            CorsSetting::One(value) => Cors::List(vec![value.trim().to_owned()]),
            CorsSetting::Many(list) if list.is_empty() => Cors::Off,
            CorsSetting::Many(list) => {
                Cors::List(list.into_iter().map(|v| v.trim().to_owned()).collect())
            }
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            secret: Secret::ephemeral(),
            csrf: true,
            session: SessionOptions::default(),
            tz_offset: 0,
            http_timeout: Duration::from_secs(10),
            mail: MailConfig::default(),
            locale: "uk".to_owned(),
            trust_proxy: false,
            cors: Cors::Off,
            api_title: "API".to_owned(),
            api_version: "1.0.0".to_owned(),
            api_docs: false,
        }
    }
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("root", &self.root)
            .field("addr", &self.addr)
            .field("database", &self.database)
            .field("dev", &self.dev)
            .field("app", &self.app)
            .field("native", &self.native)
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
    app: Option<AppSection>,
    mail: Option<MailSection>,
    api: Option<ApiSection>,
}

/// `[api]` у `rhaix.toml`.
#[derive(Debug, Default, serde::Deserialize)]
struct ApiSection {
    cors: Option<CorsSetting>,
    title: Option<String>,
    version: Option<String>,
    openapi: Option<bool>,
}

/// `[api]` → поля `AppConfig`, що стосуються опису API.
fn apply_api_section(app: &mut AppConfig, file: &ConfigFile, root: &Path) {
    app.cors = cors(file);
    let api = file.api.as_ref();
    app.api_title = api
        .and_then(|a| a.title.clone())
        // Без назви — ім'я теки проєкту: краще, ніж безлике «API».
        .or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "API".to_owned());
    if let Some(version) = api.and_then(|a| a.version.clone()) {
        app.api_version = version;
    }
    app.api_docs = api.and_then(|a| a.openapi).unwrap_or(false);
}

#[derive(Debug, Default, serde::Deserialize)]
struct ServerSection {
    port: Option<u16>,
    /// Довіряти `X-Forwarded-For`. Лише за власним проксі: без нього цей
    /// заголовок підставляє будь-хто, і `req.ip` став би що завгодно.
    trust_proxy: Option<bool>,
}

/// `[app]` у `rhaix.toml`.
#[derive(Debug, Default, serde::Deserialize)]
struct AppSection {
    /// Ключ підпису сесії. У репозиторії йому не місце — краще `RHAIX_SECRET`.
    secret: Option<String>,
    csrf: Option<bool>,
    session_cookie: Option<String>,
    /// Скільки живе сесія, у днях.
    session_days: Option<i64>,
    /// `Secure` на cookie сесії. За замовчуванням — у продакшні так.
    session_secure: Option<bool>,
    /// Зсув показу дат: `"+03:00"`.
    tz_offset: Option<String>,
    /// Таймаут `http`, у секундах.
    http_timeout: Option<u64>,
    /// Мова за замовчуванням для `t(...)`.
    locale: Option<String>,
}

/// `[mail]` у `rhaix.toml`.
#[derive(Debug, Default, serde::Deserialize)]
struct MailSection {
    from: Option<String>,
    smtp_host: Option<String>,
    smtp_port: Option<u16>,
    smtp_user: Option<String>,
    smtp_pass: Option<String>,
}

/// Зібрати `[app]` з файлу, оточення й режиму запуску.
fn app_config(
    file: Option<&AppSection>,
    mail: Option<&MailSection>,
    root: &Path,
    dev: bool,
    persist: bool,
) -> AppConfig {
    let mut app = AppConfig {
        secret: resolve_secret(file.and_then(|a| a.secret.as_deref()), root, dev, persist),
        ..AppConfig::default()
    };
    // `Secure` на cookie: у продакшні так, у dev ні — інакше cookie не поїде
    // на http://localhost.
    app.session.secure = !dev;
    if let Some(section) = file {
        if let Some(flag) = section.csrf {
            app.csrf = flag;
        }
        if let Some(name) = &section.session_cookie {
            app.session.cookie = name.clone();
        }
        if let Some(days) = section.session_days {
            app.session.max_age = days.clamp(1, 365) * 86_400;
        }
        if let Some(flag) = section.session_secure {
            app.session.secure = flag;
        }
        if let Some(raw) = &section.tz_offset {
            match parse_tz_offset(raw) {
                Some(minutes) => app.tz_offset = minutes,
                None => tracing::warn!("`tz_offset = \"{raw}\"` не схоже на зсув на кшталт +03:00"),
            }
        }
        if let Some(seconds) = section.http_timeout {
            app.http_timeout = Duration::from_secs(seconds.clamp(1, 300));
        }
        if let Some(locale) = &section.locale {
            app.locale = locale.clone();
        }
    }
    if let Some(m) = mail {
        app.mail = MailConfig {
            from: m.from.clone().unwrap_or_default(),
            host: m.smtp_host.clone().unwrap_or_default(),
            port: m.smtp_port.unwrap_or(587),
            user: m.smtp_user.clone().unwrap_or_default(),
            password: m.smtp_pass.clone().unwrap_or_default(),
        };
    }
    app
}

/// Ключ, якого фреймворк не читає, — у лог при старті (див. `config_keys`).
fn warn_unknown_keys(text: &str) {
    for unknown in config_keys::unknown_keys(text) {
        tracing::warn!("{} (рядок {})", unknown.message(), unknown.line);
    }
}

fn cors(file: &ConfigFile) -> Cors {
    file.api
        .as_ref()
        .and_then(|api| api.cors.clone())
        .map(CorsSetting::into_cors)
        .unwrap_or_default()
}

fn trust_proxy(file: &ConfigFile) -> bool {
    file.server
        .as_ref()
        .and_then(|server| server.trust_proxy)
        .unwrap_or(false)
}

/// Знайти ключ підпису cookie.
///
/// Порядок такий: `RHAIX_SECRET` → `[app] secret` → файл `.rhaix-secret` у
/// корені проєкту (лише `rhaix dev`) → випадковий на час життя процесу.
///
/// `persist` вимикає передостанній крок: `rhaix check` і зібраний бінарник
/// нічого в проєкт не пишуть.
///
/// Останній варіант робочий, але має наслідок, про який треба сказати вголос:
/// після перезапуску всі сесії стають недійсними, а дві копії застосунку за
/// балансиром не розуміють cookie одна одної. Тому в продакшні про це
/// попереджаємо в лог.
pub fn resolve_secret(from_file: Option<&str>, root: &Path, dev: bool, persist: bool) -> Secret {
    if let Ok(value) = std::env::var("RHAIX_SECRET") {
        if !value.trim().is_empty() {
            return Secret::new(value.into_bytes());
        }
    }
    if let Some(value) = from_file {
        if !value.trim().is_empty() {
            return Secret::new(value.as_bytes().to_vec());
        }
    }
    if persist {
        // У розробці сесія має переживати перезапуск сервера: інакше кожне
        // збереження файлу розлогінювало б розробника.
        let path = root.join(".rhaix-secret");
        if let Ok(existing) = fs::read_to_string(&path) {
            if !existing.trim().is_empty() {
                return Secret::new(existing.trim().as_bytes().to_vec());
            }
        }
        let generated = rhaix_script::random_token(32);
        if fs::write(&path, &generated).is_ok() {
            tracing::info!("створено `.rhaix-secret` — додайте його до .gitignore");
            return Secret::new(generated.into_bytes());
        }
    }
    if !dev {
        tracing::warn!(
            "секрет не заданий: сесії не переживуть перезапуск. \
             Поставте змінну RHAIX_SECRET або `[app] secret` у rhaix.toml"
        );
    }
    Secret::ephemeral()
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
            app: AppConfig::default(),
            native: Native::default(),
        }
    }

    /// Конфіг для зібраного бінарника: файли беруться з вшитої таблиці,
    /// режим — продакшн, коренем є порожній шлях.
    ///
    /// Цим користується код, який генерує `rhaix build`.
    pub fn embedded(files: Arc<dyn Files>, port: Option<u16>) -> anyhow::Result<Self> {
        let raw = files.read_text(Path::new("rhaix.toml")).unwrap_or_default();
        warn_unknown_keys(&raw);
        let file: ConfigFile =
            toml::from_str(&raw).map_err(|err| anyhow::anyhow!("rhaix.toml: {err}"))?;
        let port = port
            .or_else(|| file.server.as_ref().and_then(|s| s.port))
            .unwrap_or(3000);

        let mut app = app_config(
            file.app.as_ref(),
            file.mail.as_ref(),
            Path::new(""),
            false,
            false,
        );
        app.trust_proxy = trust_proxy(&file);
        apply_api_section(&mut app, &file, Path::new(""));
        Ok(Self {
            root: PathBuf::new(),
            addr: SocketAddr::from(([0, 0, 0, 0], port)),
            database: file.db,
            dev: false,
            files,
            embedded: true,
            app,
            native: Native::default(),
        })
    }

    /// Те саме, але для продакшну: без стеження за файлами й без клієнта
    /// живого перезавантаження.
    pub fn load_release(root: impl Into<PathBuf>, port: Option<u16>) -> anyhow::Result<Self> {
        // Не `load` із подальшим `dev = false`: від режиму залежить і секрет,
        // і `Secure` на cookie, а вони вирішуються під час читання конфігу.
        Self::load_inner(root, port, false)
    }

    /// Прочитати `rhaix.toml`, якщо він є.
    ///
    /// Порт із командного рядка сильніший за файл: під час розробки часто треба
    /// підняти другий сервер, не редагуючи конфіг.
    pub fn load(root: impl Into<PathBuf>, port: Option<u16>) -> anyhow::Result<Self> {
        Self::load_inner(root, port, true)
    }

    /// Конфіг для `rhaix check`: те саме читання, але без побічних ефектів —
    /// перевірка проєкту не має нічого в ньому створювати.
    pub fn load_for_check(root: impl Into<PathBuf>) -> anyhow::Result<Self> {
        Self::load_inner_with(root, None, true, false)
    }

    fn load_inner(root: impl Into<PathBuf>, port: Option<u16>, dev: bool) -> anyhow::Result<Self> {
        Self::load_inner_with(root, port, dev, true)
    }

    fn load_inner_with(
        root: impl Into<PathBuf>,
        port: Option<u16>,
        dev: bool,
        persist_secret: bool,
    ) -> anyhow::Result<Self> {
        let root = root.into();
        let path = root.join("rhaix.toml");
        let file: ConfigFile = if path.is_file() {
            let text = fs::read_to_string(&path)?;
            // `rhaix check` скаже про це сам, попередженням із рядком.
            if persist_secret {
                warn_unknown_keys(&text);
            }
            toml::from_str(&text).map_err(|err| anyhow::anyhow!("rhaix.toml: {err}"))?
        } else {
            ConfigFile::default()
        };

        let port = port
            .or_else(|| file.server.as_ref().and_then(|s| s.port))
            .unwrap_or(3000);

        let mut app = app_config(
            file.app.as_ref(),
            file.mail.as_ref(),
            &root,
            dev,
            dev && persist_secret,
        );
        app.trust_proxy = trust_proxy(&file);
        apply_api_section(&mut app, &file, &root);
        Ok(Self {
            root,
            addr: SocketAddr::from(([127, 0, 0, 1], port)),
            database: file.db,
            dev,
            files: DiskFiles::shared(),
            embedded: false,
            app,
            native: Native::default(),
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

    /// `api/` — маршрути, що віддають JSON зовнішнім клієнтам (SYNTAX 6.7).
    pub fn api_dir(&self) -> PathBuf {
        self.root.join("api")
    }

    pub fn layout_path(&self) -> PathBuf {
        self.layout_named("main")
    }

    /// `layouts/<name>.rhx`. Ім'я звіряється: воно приходить зі скрипта
    /// користувача, і `page.layout = "../../etc/passwd"` не має нікуди вести.
    pub fn layout_named(&self, name: &str) -> PathBuf {
        self.root.join("layouts").join(format!("{name}.rhx"))
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
    /// Маршрут із `api/`: JSON замість HTML, без layout, без сесії й без CSRF.
    ///
    /// Сесії тут немає **навмисно**. Якби маршрут без CSRF-перевірки все ж
    /// читав cookie, то `POST /api/delete` зі стороннього сайту виконався б від
    /// імені залогіненого користувача — класична CSRF-дірка. Тому автентифікація
    /// в `api/` можлива лише за токеном із заголовка, і зловити чужу сесію
    /// просто нема звідки.
    Api,
}

impl RouteKind {
    /// Чи віддає цей маршрут JSON замість HTML.
    fn is_api(self) -> bool {
        matches!(self, RouteKind::Api)
    }
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
    /// Клієнт для `http` у скриптах: пул з'єднань один на застосунок.
    http: Http,
    /// Пошта: SMTP або dev-лог, один на застосунок.
    mail: Mail,
    /// Переклади з `locales/*.toml`, спільні для всіх запитів.
    catalog: Arc<Catalog>,
    /// `live.send(...)` у скриптах і канал, з якого читає `/_rhaix/live`.
    live: Live,
    live_tx: broadcast::Sender<Arc<LiveMessage>>,
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
    // Зсув дат глобальний на процес: `date()` у будь-якому файлі показує час
    // у поясі застосунку, а не в UTC. Ставимо його тут, а не в `serve`, щоб
    // застосунок, зібраний у тесті, поводився так само, як запущений.
    rhaix_script::set_tz_offset(config.app.tz_offset);

    let mut routes = scan_pages(config.files.as_ref(), &config.pages_dir())?;
    routes.extend(scan_partials(
        config.files.as_ref(),
        &config.partials_dir(),
    )?);
    routes.extend(scan_api(config.files.as_ref(), &config.api_dir())?);

    let database = match &config.database {
        Some(settings) => {
            // Відносний шлях у `rhaix.toml` — відносно **кореня проєкту**, а не
            // теки, з якої запустили процес. Інакше `rhaix dev ../app` створює
            // базу не там, і після деплою це виглядає як зникнення даних.
            let url = resolve_db_url(&config.root, &settings.url);
            let driver = settings.driver.clone();
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

            // Відкриття бази й міграції — в окремому потоці. Синхронний драйвер
            // Postgres усередині крутить власний рантайм через `block_on`, а
            // старт сервера вже в контексті tokio: виклик звідти панікує з
            // «Cannot start a runtime from within a runtime». Запитний шлях
            // цього не має — він іде в `spawn_blocking`, поза контекстом.
            std::thread::scope(|scope| {
                scope
                    .spawn(|| -> anyhow::Result<Database> {
                        let database = Database::open(&driver, &url)
                            .map_err(|err| anyhow::anyhow!("{err}"))?;
                        // Сервер, який піднявся, завжди має схему, яку очікують
                        // сторінки.
                        let applied = database
                            .migrate(&migrations)
                            .map_err(|err| anyhow::anyhow!("{err}"))?;
                        for name in &applied {
                            tracing::info!("міграція застосована: {name}");
                        }
                        Ok(database)
                    })
                    .join()
                    .map_err(|_| anyhow::anyhow!("потік ініціалізації бази впав"))?
            })?
        }
        None => Database::unconfigured(),
    };
    // Спільні функції проєкту підключаються один раз при старті. Файли беруться
    // через ті самі `Files`, що й шаблони: у зібраному бінарнику їх на диску немає.
    let mut engine = build_engine(Limits::default());
    engine.set_module_resolver(scripts::ScriptResolver::new(
        config.root.clone(),
        config.files.clone(),
        config.dev,
    ));
    // Власні функції — до спільних скриптів: ті можуть їх викликати.
    if let Some(register) = &config.native.0 {
        register(&mut engine);
    }
    scripts::load_globals(&mut engine, &config.root, config.files.as_ref())?;
    let engine = Arc::new(engine);

    let (live_tx, _) = broadcast::channel::<Arc<LiveMessage>>(LIVE_BUFFER);
    let publisher = live_tx.clone();
    let live = Live::new(Arc::new(move |topic: &str, detail: &str| {
        // Немає підписників — не помилка: сторінку ніхто не тримає відкритою.
        let _ = publisher.send(Arc::new(LiveMessage {
            topic: topic.to_owned(),
            detail: detail.to_owned(),
        }));
    }));

    let state = AppState {
        config: config.clone(),
        engine: engine.clone(),
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
        http: Http::new(config.app.http_timeout),
        mail: Mail::new(config.app.mail.clone()),
        catalog: Arc::new(load_catalog(&config)),
        live,
        live_tx,
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

    // Невідома адреса під `/api/` має відповідати машинно. Без цього запит
    // провалювався б у загальний fallback і клієнт отримував би HTML-сторінку
    // 404 — рівно та невідповідність, через яку розбір падає замість пояснення.
    // Маршрут реєструється, лише якщо `api/` узагалі є: інакше він перехопив би
    // цілком законний `public/api/…`.
    // Опис API — лише якщо попросили (`[api] openapi = true`): перелік
    // маршрутів і полів — не те, що кожен застосунок хоче показувати світу.
    // Збирається при старті; у dev — на кожен запит, щоб правка файлу була
    // видна одразу.
    let router = if config.app.api_docs && routes.iter().any(|route| route.kind.is_api()) {
        let docs_config = config.clone();
        let frozen = if config.dev {
            None
        } else {
            Some(openapi::openapi(&config)?.to_string())
        };
        router.route(
            "/api/openapi.json",
            axum::routing::get(move || {
                let body = match &frozen {
                    Some(body) => Ok(body.clone()),
                    None => openapi::openapi(&docs_config).map(|spec| spec.to_string()),
                };
                std::future::ready(match body {
                    Ok(body) => (
                        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                        body,
                    )
                        .into_response(),
                    Err(err) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        json_body(&format!("опис API не зібрався: {err}")),
                    )
                        .into_response(),
                })
            }),
        )
    } else {
        router
    };

    let router = if routes.iter().any(|route| route.kind.is_api()) {
        router.route(
            "/api/{*rest}",
            any(|| async {
                (StatusCode::NOT_FOUND, json_body("маршрут не знайдено")).into_response()
            }),
        )
    } else {
        router
    };

    // CORS — шаром над усім роутером, а не в `serve_page`: так ним накриті й
    // відповіді маршрутів, і помилки, і 404 під `/api/`, а preflight
    // (`OPTIONS`) відповідається ще до того, як запит дійде до скрипта.
    let router = if state.config.app.cors != Cors::Off {
        let cors = Arc::new(state.config.app.cors.clone());
        router.layer(axum::middleware::from_fn(
            move |request: HttpRequest<Body>, next: axum::middleware::Next| {
                let cors = cors.clone();
                async move { api_cors(&cors, request, next).await }
            },
        ))
    } else {
        router
    };

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

    // Живі оновлення: одна SSE-підписка на сторінку, на ті теми, які вона
    // слухає. Лише сигнал «тема змінилась» — дані сторінка перезапитує сама.
    let live_tx = state.live_tx.clone();
    let router = router.route(
        LIVE_ROUTE,
        axum::routing::get(move |uri: axum::http::Uri| {
            let topics: std::collections::HashSet<String> = uri
                .query()
                .map(parse_urlencoded)
                .and_then(|query| query.get("topics").cloned())
                .unwrap_or_default()
                .split(',')
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty())
                .collect();
            let stream = BroadcastStream::new(live_tx.subscribe()).filter_map(move |message| {
                // `Lagged` — пропущені повідомлення; клієнт надолужить сам.
                let message = message.ok()?;
                if !topics.contains(&message.topic) {
                    return None;
                }
                let data = format!(
                    "{{\"topic\":{},\"detail\":{}}}",
                    serde_json::Value::String(message.topic.clone()),
                    message.detail
                );
                Some(Ok::<Event, std::convert::Infallible>(
                    Event::default().data(data),
                ))
            });
            std::future::ready(
                (
                    // Проксі на кшталт nginx буферизує відповідь — і події
                    // доходили б пачками з запізненням.
                    [("X-Accel-Buffering", "no")],
                    Sse::new(stream).keep_alive(KeepAlive::default()),
                )
                    .into_response(),
            )
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

    // Вбудований UI — окремим файлом, щоб проєкт міг підмінити саме його,
    // не чіпаючи ядро (`public/rhaix-ui.js`, SYNTAX 7.7).
    let router = router.route(
        UI_ROUTE,
        axum::routing::get(|| async {
            (
                [
                    (
                        header::CONTENT_TYPE,
                        "application/javascript; charset=utf-8",
                    ),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
                UI_JS,
            )
        }),
    );

    // htmx їде з бінарника, а не з CDN: інакше застосунок не працював би без
    // інтернету, і «один файл» було б перебільшенням. Кеш довгий — вміст
    // прив'язаний до версії фреймворку.
    let router = router.route(
        HTMX_ROUTE,
        axum::routing::get(|| async {
            (
                [
                    (
                        header::CONTENT_TYPE,
                        "application/javascript; charset=utf-8",
                    ),
                    (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
                ],
                HTMX_JS,
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
            // Без `Cache-Control` браузер кешує файл евристично — на частку
            // часу від останньої зміни. Живе перезавантаження тоді оновлює
            // сторінку, а стара CSS лишається. `no-cache` — щоразу перепитати
            // (з `Last-Modified` це дешеві 304), тож правка видна одразу.
            router.fallback_service(
                ServiceBuilder::new()
                    .layer(SetResponseHeaderLayer::if_not_present(
                        header::CACHE_CONTROL,
                        HeaderValue::from_static("no-cache"),
                    ))
                    .service(files),
            )
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

/// Просканувати `api/`: `orders.rhx` → `/api/orders`, `orders/[id].rhx` →
/// `/api/orders/{id}`.
///
/// Регістр, на відміну від `partials/`, не змінюється: у `api/` файл названий
/// так, як має виглядати URL, і тиха зміна ламала б `ordersById.rhx`.
pub fn scan_api(files: &dyn Files, dir: &Path) -> anyhow::Result<Vec<PageRoute>> {
    let mut routes = scan_pages(files, dir)?;
    for route in &mut routes {
        route.kind = RouteKind::Api;
        // `index.rhx` дає `/`, тобто корінь самого API.
        route.pattern = match route.pattern.as_str() {
            "/" => "/api".to_owned(),
            pattern => format!("/api{pattern}"),
        };
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
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip());
    let mut data = match collect_request(params, request).await {
        Ok(data) => data,
        Err(err) => return err.respond(kind),
    };
    data.ip = client_ip(peer, &data.headers, state.config.app.trust_proxy);
    // Куди `upload.save(...)` пише файли — відносно кореня проєкту, як і база.
    data.upload_root = state.config.root.clone();
    let hx_request = data.hx_request;

    // Скрипт користувача синхронний і може ходити в БД, тому виконується на
    // окремому потоці; рушій спільний (`sync`-збірка Rhai), шаблон — теж.
    let file_for_error = file.clone();
    let outcome = tokio::task::spawn_blocking(move || render_page(&state, &file, kind, data)).await;

    let (body, response_state) = match outcome {
        Ok(Ok(result)) => result,
        Ok(Err(err)) => return err.respond(kind),
        Err(err) => {
            tracing::error!("рендер не завершився: {err}");
            return PageError::Io {
                file: file_for_error,
                message: format!("рендер не завершився: {err}"),
            }
            .respond(kind);
        }
    };

    build_response(body, response_state, hx_request, kind)
}

/// Чи стосується шлях `api/`.
fn is_api_path(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

/// CORS для `api/`: відповідь на preflight і дозвіл читати звичайні відповіді.
///
/// Сторінки й статика сюди не потрапляють: сторінки живуть на cookie-сесії, і
/// дозвіл чужому сайту читати їх означав би віддати йому все, що бачить
/// залогінений користувач.
async fn api_cors(
    cors: &Cors,
    request: HttpRequest<Body>,
    next: axum::middleware::Next,
) -> Response {
    if !is_api_path(request.uri().path()) {
        return next.run(request).await;
    }
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let allowed = cors.allow(origin.as_deref());

    // Preflight: браузер питає дозволу перед запитом із заголовком
    // `Authorization` чи JSON-тілом. Відповідаємо самі, до скрипта.
    let is_preflight = request.method() == axum::http::Method::OPTIONS
        && request
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD);
    if is_preflight {
        let mut response = StatusCode::NO_CONTENT.into_response();
        let headers = response.headers_mut();
        if let Some(allow) = &allowed {
            insert_header(headers, "Access-Control-Allow-Origin", allow);
            insert_header(
                headers,
                "Access-Control-Allow-Methods",
                "GET, POST, PUT, PATCH, DELETE, OPTIONS",
            );
            let asked = request
                .headers()
                .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("Authorization, Content-Type")
                .to_owned();
            insert_header(headers, "Access-Control-Allow-Headers", &asked);
            insert_header(headers, "Access-Control-Max-Age", "600");
        }
        headers.insert(header::VARY, HeaderValue::from_static("Origin"));
        return response;
    }

    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    if let Some(allow) = allowed {
        insert_header(headers, "Access-Control-Allow-Origin", &allow);
        // Без цього скрипт на чужому сайті не прочитає, скільки чекати після 429.
        insert_header(
            headers,
            "Access-Control-Expose-Headers",
            "Retry-After, Location",
        );
    }
    // Відповідь залежить від `Origin` — кеш не має віддати її іншому сайту.
    if !matches!(cors, Cors::Any) {
        headers.append(header::VARY, HeaderValue::from_static("Origin"));
    }
    response
}

/// IP-адреса клієнта для `req.ip`.
///
/// За замовчуванням — адреса TCP-з'єднання. За власним проксі вона завжди
/// адреса проксі, тож із `trust_proxy` беремо **останній** запис
/// `X-Forwarded-For`: його дописав наш проксі, а все, що лівіше, прийшло від
/// клієнта і може бути вигадане. Без `trust_proxy` заголовок ігнорується
/// повністю — інакше ліміт спроб обходився б одним рядком у запиті.
fn client_ip(
    peer: Option<std::net::IpAddr>,
    headers: &std::collections::BTreeMap<String, String>,
    trust_proxy: bool,
) -> String {
    if trust_proxy {
        let forwarded = headers
            .get("x-forwarded-for")
            .and_then(|raw| raw.rsplit(',').map(str::trim).find(|ip| !ip.is_empty()))
            .or_else(|| headers.get("x-real-ip").map(|ip| ip.trim()))
            .and_then(|ip| ip.parse::<std::net::IpAddr>().ok());
        if let Some(ip) = forwarded {
            return ip.to_string();
        }
    }
    peer.map(|ip| ip.to_string()).unwrap_or_default()
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

    let content_type = headers
        .get("content-type")
        .map(String::as_str)
        .unwrap_or("");
    let is_urlencoded = content_type.starts_with("application/x-www-form-urlencoded");

    // Форма з файлами: текстові поля йдуть у `form`, файли — у `files`.
    let mut form = std::collections::BTreeMap::new();
    let mut files: std::collections::BTreeMap<String, Vec<UploadData>> =
        std::collections::BTreeMap::new();
    if is_urlencoded {
        form = parse_urlencoded(&body_text);
    } else if content_type.starts_with("multipart/form-data") {
        if let Some(boundary) = multipart::boundary(content_type) {
            for part in multipart::parse(&bytes, &boundary) {
                if part.is_file() {
                    files
                        .entry(part.name.clone())
                        .or_default()
                        .push(UploadData {
                            filename: part.filename.unwrap_or_default(),
                            content_type: part.content_type.unwrap_or_default(),
                            data: std::sync::Arc::new(part.data),
                        });
                } else {
                    form.insert(part.name, String::from_utf8_lossy(&part.data).into_owned());
                }
            }
        }
    }

    // Три різні речі, які легко сплутати (саме так і сталося до 1.2.5):
    // - `hx_request` — запит прийшов від htmx узагалі;
    // - `is_boosted` — це звичайне посилання чи форма під `<body hx-boost>`;
    // - `is_htmx` — відповідь має бути фрагментом без layout.
    // Boosted-запит і відновлення історії htmx свопить у ВЕСЬ `<body>`: якщо
    // віддати їм фрагмент, зникають меню, `#main` і контейнер тостів, і
    // застосунок ламається до перезавантаження. Тому фрагмент — лише для
    // явних hx-get/hx-post із власною ціллю.
    let hx_request = headers.contains_key("hx-request");
    let is_boosted = headers.contains_key("hx-boosted");
    let is_history_restore = headers.contains_key("hx-history-restore-request");
    let is_htmx = hx_request && !is_boosted && !is_history_restore;

    Ok(RequestData {
        method: parts.method.as_str().to_owned(),
        path: parts.uri.path().to_owned(),
        url: parts
            .uri
            .path_and_query()
            .map(|pq| pq.as_str().to_owned())
            .unwrap_or_else(|| parts.uri.path().to_owned()),
        params: params
            .iter()
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect(),
        query: parts.uri.query().map(parse_urlencoded).unwrap_or_default(),
        form,
        files,
        is_htmx,
        hx_request,
        is_boosted,
        headers,
        cookies,
        body: body_text,
        upload_root: std::path::PathBuf::new(),
        // Ставить `serve_page`: адреса з'єднання й довіра до проксі — не тіло запиту.
        ip: String::new(),
    })
}

/// Скласти HTTP-відповідь із того, що попросив скрипт.
fn build_response(
    body: String,
    state: ResponseData,
    hx_request: bool,
    kind: RouteKind,
) -> Response {
    let mut status = StatusCode::from_u16(state.status).unwrap_or(StatusCode::OK);
    if let Some(file) = &state.download {
        return download_response(file, &state, status);
    }
    let mut response = Response::new(Body::from(body));

    {
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            if kind.is_api() {
                HeaderValue::from_static("application/json; charset=utf-8")
            } else {
                HeaderValue::from_static("text/html; charset=utf-8")
            },
        );
        // `Vary: HX-Request` має сенс лише там, де та сама адреса віддає то
        // сторінку, то фрагмент. У `api/` відповідь одна, і зайвий `Vary`
        // тільки дробив би кеш.
        if !kind.is_api() {
            // Vary самого по собі мало: деякі CDN його ігнорують і можуть віддати
            // фрагмент замість сторінки (RISKS 2.9), тому HTMX-відповіді не кешуємо.
            // Відповідь залежить від усіх трьох заголовків (див. collect_request).
            headers.insert(
                header::VARY,
                HeaderValue::from_static("HX-Request, HX-Boosted, HX-History-Restore-Request"),
            );
            if hx_request {
                headers.insert(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("private, no-store"),
                );
            }
        }

        for (name, value) in &state.headers {
            insert_header(headers, name, value);
        }
        for cookie in &state.cookies {
            if let Ok(value) = HeaderValue::from_str(cookie) {
                headers.append(header::SET_COOKIE, value);
            }
        }
        // Відповідь із cookie сесії — особиста. Спільний кеш (CDN, проксі), що
        // збереже її, роздасть чужу сесію всім наступним відвідувачам. Явне
        // рішення скрипта (`res.header("Cache-Control", …)`) лишаємо в силі.
        if !state.cookies.is_empty() && !headers.contains_key(header::CACHE_CONTROL) {
            headers.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, no-store"),
            );
        }
        if let Some(trigger) = rhaix_script::triggers_header(&state.triggers) {
            insert_header(headers, "HX-Trigger", &trigger);
            insert_header(headers, "Access-Control-Expose-Headers", "HX-Trigger");
        }
        if state.refresh {
            insert_header(headers, "HX-Refresh", "true");
        }
        if let Some(url) = &state.redirect {
            // htmx сам не піде за 303 — для нього потрібен заголовок (і для
            // boosted-форми теж), а для звичайного заходу — звичайний редірект.
            if hx_request {
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

/// Відповідь-файл для `res.download(...)`.
fn download_response(
    file: &rhaix_script::Download,
    state: &ResponseData,
    status: StatusCode,
) -> Response {
    let content_type = file
        .content_type
        .clone()
        .unwrap_or_else(|| content_type_for(&file.filename, file.is_text));

    // Excel відкриває CSV без BOM у системному кодуванні — і кирилиця
    // перетворюється на «РђР±РІ». BOM — стандартний спосіб сказати «це UTF-8»;
    // інші програми його просто пропускають.
    let mut bytes = file.bytes.as_ref().clone();
    if file.is_text && content_type.starts_with("text/csv") && !bytes.starts_with(BOM) {
        bytes.splice(0..0, BOM.iter().copied());
    }

    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    insert_header(headers, "Content-Type", &content_type);
    insert_header(
        headers,
        "Content-Disposition",
        &content_disposition(&file.filename),
    );
    // Тип задаємо ми; браузер не має вгадувати в тексті HTML.
    insert_header(headers, "X-Content-Type-Options", "nosniff");
    for (name, value) in &state.headers {
        insert_header(headers, name, value);
    }
    for cookie in &state.cookies {
        if let Ok(value) = HeaderValue::from_str(cookie) {
            headers.append(header::SET_COOKIE, value);
        }
    }
    // Вивантаження — зазвичай чиїсь дані: у спільному кеші їм не місце.
    if !headers.contains_key(header::CACHE_CONTROL) {
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
    }
    response
}

const BOM: &[u8] = b"\xEF\xBB\xBF";

/// Тип файлу за розширенням. Лише те, що реально віддають застосунки;
/// решта — `application/octet-stream`, і браузер просто збереже файл.
fn content_type_for(filename: &str, is_text: bool) -> String {
    let extension = Path::new(filename)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let base = match extension.as_str() {
        "csv" => "text/csv",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "html" | "htm" => "text/html",
        "ics" => "text/calendar",
        "json" => "application/json",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ if is_text => "text/plain",
        _ => "application/octet-stream",
    };
    let textual = base.starts_with("text/") || base.ends_with("json") || base.ends_with("xml");
    if is_text && textual {
        format!("{base}; charset=utf-8")
    } else {
        base.to_owned()
    }
}

/// `Content-Disposition` з ім'ям, яке переживе будь-який браузер.
///
/// Ім'я в заголовку — ASCII. Тому два варіанти: `filename=` — запасний, де
/// все не-ASCII замінено на `_`, і `filename*=UTF-8''…` (RFC 6266) із
/// справжнім ім'ям — його читають усі сучасні браузери. Роздільники шляху й
/// керівні символи прибираються з обох: ім'я від користувача (`звіт/../x`)
/// не має вказувати, куди зберегти файл.
fn content_disposition(filename: &str) -> String {
    let clean: String = filename
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | '"') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let fallback: String = clean
        .chars()
        .map(|c| if c.is_ascii() { c } else { '_' })
        .collect();
    let mut encoded = String::new();
    for byte in clean.bytes() {
        let keep = byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'!' | b'#' | b'$' | b'&' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'
            );
        if keep {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
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
/// Сесія, перевірка CSRF і рендер — у цьому порядку.
///
/// Перевірка стоїть **перед** `middleware.rhx`: підроблений запит не має
/// доходити до жодного рядка логіки застосунку.
fn render_page(
    state: &AppState,
    file: &Path,
    kind: RouteKind,
    data: RequestData,
) -> Result<(String, ResponseData), PageError> {
    let _deadline = Deadline::new(SCRIPT_BUDGET);
    let app = &state.config.app;

    // `api/` не бачить cookie взагалі: див. коментар при `RouteKind::Api`.
    // Порожня сесія, а не «сесія, яку не перевіряють» — інакше досить забути
    // одну перевірку, щоб дірка відкрилась сама.
    let cookie = if kind.is_api() {
        None
    } else {
        data.cookies.get(&app.session.cookie).map(String::as_str)
    };
    let session = Session::restore(cookie, app.secret.bytes());
    let csrf = Csrf::new(session.clone(), app.csrf);
    // Мова — стан запиту: `set_locale` у middleware має впливати на сторінку.
    let i18n = I18n::new(state.catalog.clone(), app.locale.clone());
    // `t(...)` — вільна функція, тому переклади прив'язуються до потоку запиту
    // (рендер іде в `spawn_blocking`: один запит — один потік).
    let _locale = LocaleScope::new(i18n.clone());

    // CSRF захищає cookie-сесію. У `api/` сесії немає, тому й захищати нечого:
    // токен із заголовка браузер до чужого запиту не додасть.
    if is_mutating(&data.method) && !kind.is_api() {
        // Поле форми або заголовок: другий варіант потрібен для `hx-headers`
        // і для запитів, у яких тіло — не форма.
        let supplied = data
            .form
            .get(CSRF_FIELD)
            .or_else(|| data.headers.get(CSRF_HEADER));
        if !csrf.verify(supplied.map(String::as_str)) {
            return Err(PageError::Forbidden {
                path: data.path.clone(),
            });
        }
    }

    let hx_request = data.hx_request;
    let is_get = data.method == "GET";
    let url = data.url.clone();
    let (mut body, mut response) = render_inner(state, file, kind, data, &session, &csrf, &i18n)?;
    if response.download.is_some() && hx_request {
        route_download_past_htmx(&mut response, is_get, &url);
    }
    if !kind.is_api() {
        body = carry_flash(&session, &mut response, body, hx_request);
    }
    // Cookie ставиться, лише якщо сесію справді змінювали — інакше кожна
    // сторінка тягла б за собою `Set-Cookie` і псувала кешування.
    // `api/` не видає cookie ніколи: сесії він не читає, тож і писати її означало
    // б віддати клієнтові стан, яким наступний запит однаково не скористається.
    if !kind.is_api() {
        if let Some(cookie) = session.cookie(app.secret.bytes(), &app.session) {
            response.cookies.push(cookie);
        }
    }
    Ok((body, response))
}

/// Файл на htmx-запит: htmx не вміє зберігати файли, він вставив би вміст
/// CSV текстом у сторінку. Найчастіше це звичайне посилання «Експорт» під
/// `<body hx-boost>` — автор про htmx і не думав.
///
/// На GET відповідаємо `HX-Redirect` на ту саму адресу: htmx робить
/// `location.href = …`, браузер отримує вже звичайну відповідь з
/// `Content-Disposition: attachment` і зберігає файл, лишаючись на сторінці.
/// Скрипт виконається вдруге — для GET, що нічого не змінює, це ціна
/// правильного завантаження. Тости першого проходу відкидаємо: другий їх
/// повторить, і вони дочекаються наступної сторінки у flash.
///
/// Не-GET так не повториш (редірект став би GET без тіла форми), тож файл
/// іде як є, але з `HX-Reswap: none` — сторінку хоч не зіпсує — і з
/// попередженням у лозі, як це виправити.
fn route_download_past_htmx(response: &mut ResponseData, is_get: bool, url: &str) {
    if is_get {
        response.download = None;
        response.redirect = Some(url.to_owned());
        take_toasts(response);
    } else {
        tracing::warn!(
            "{url}: res.download() на htmx-запит не GET — файл не дійде до користувача. \
             Віддавайте файли на GET-посилання або поставте hx-boost=\"false\" на форму"
        );
        response
            .headers
            .push(("HX-Reswap".to_owned(), "none".to_owned()));
    }
}

/// Ключ сесії для тостів, що мають пережити редірект.
const FLASH_KEY: &str = "_flash";

/// Тости через редірект (flash).
///
/// `hx.toast("Вітаємо"); res.redirect("/admin")` — найчастіше поєднання, і без
/// цього тост ніхто не бачить: htmx малює подію з `HX-Trigger`, а наступним
/// рядком робить `location.href = HX-Redirect`, і сторінка йде геть разом із
/// тостом (звичайна форма без htmx узагалі отримує 303 без жодного тосту).
///
/// Тому тости відповіді, що веде деінде (`res.redirect`, `hx.redirect`,
/// `hx.refresh`), лягають у сесію, а перша ж відповідь без редіректу їх
/// віддає: htmx-запиту — подією `showToast`, повній сторінці — вкладеним
/// `<script type="application/json" data-rhx-toasts>`, який підхоплює `ui.js`.
fn carry_flash(
    session: &Session,
    response: &mut ResponseData,
    mut body: String,
    hx_request: bool,
) -> String {
    let pending: Vec<Dynamic> = session
        .get(FLASH_KEY)
        .try_cast::<rhai::Array>()
        .unwrap_or_default();

    // Файл — теж відповідь, у якій тост показати ніде: сторінка лишається
    // та сама, а тіло — файл. Тож він чекає наступної відповіді, як і при
    // редіректі; а вже накопичені лишаються в сесії.
    if response.redirect.is_some() || response.refresh || response.download.is_some() {
        let mut all = pending;
        all.extend(take_toasts(response));
        if !all.is_empty() {
            session.set(FLASH_KEY, Dynamic::from_array(all));
        }
        return body;
    }

    if !pending.is_empty() {
        session.remove(FLASH_KEY);
    }
    if hx_request {
        // Попереду тих, що поставила сама ця сторінка: вони сталися раніше.
        let mut triggers: Vec<(String, Dynamic)> = pending
            .into_iter()
            .map(|toast| ("showToast".to_owned(), toast))
            .collect();
        triggers.append(&mut response.triggers);
        response.triggers = triggers;
    } else {
        // Звичайне завантаження сторінки: HX-Trigger браузер не читає, тож
        // власні тости сторінки теж їдуть у тілі — інакше вони губились би.
        let mut all = pending;
        all.extend(take_toasts(response));
        if all.is_empty() {
            return body;
        }
        // JSON у <script> — `</` не має закрити тег.
        let json = rhaix_script::json_encode(&Dynamic::from_array(all)).replace("</", "<\\/");
        let tag = format!("<script type=\"application/json\" data-rhx-toasts>{json}</script>");
        match body.rfind("</body>") {
            Some(at) => body.insert_str(at, &tag),
            None => body.push_str(&tag),
        }
    }
    body
}

/// Забрати з відповіді всі `showToast`, лишивши інші події на місці.
fn take_toasts(response: &mut ResponseData) -> Vec<Dynamic> {
    let mut toasts = Vec::new();
    response.triggers.retain(|(name, detail)| {
        if name == "showToast" {
            toasts.push(detail.clone());
            false
        } else {
            true
        }
    });
    toasts
}

/// Чи може цей метод щось змінити. `GET`, `HEAD` і `OPTIONS` — ні.
fn is_mutating(method: &str) -> bool {
    !matches!(method, "GET" | "HEAD" | "OPTIONS")
}

/// Значення, повернуте скриптом, у тіло відповіді.
///
/// У `api/` мапа й масив серіалізуються в JSON самі. Без цього `return #{ ok: 1 }`
/// віддавав би Rhai-подібний `#{"ok": 1}` — рядок, який достатньо схожий на JSON,
/// щоб пройти очима, і достатньо не JSON, щоб клієнт упав. Рядок лишається як є:
/// його вже або зібрав `json_encode()`, або це навмисно не JSON.
fn body_from(value: &Dynamic, kind: RouteKind) -> String {
    if kind.is_api() && (value.is::<Map>() || value.is::<rhai::Array>()) {
        return rhaix_script::json_encode(value);
    }
    display(value)
}

fn render_inner(
    state: &AppState,
    file: &Path,
    kind: RouteKind,
    data: RequestData,
    session: &Session,
    csrf: &Csrf,
    i18n: &I18n,
) -> Result<(String, ResponseData), PageError> {
    let is_htmx = data.is_htmx;
    // Boosted-перехід і відновлення історії: повна сторінка, але htmx візьме з
    // неї лише `<body>` — `<head>` він не чіпає.
    let full_swap = data.hx_request && !data.is_htmx;
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
        &state.live,
        &state.database,
        &state.http,
        &state.mail,
        i18n,
        session,
        csrf,
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
            // middleware віддав готове тіло — сторінка не виконується взагалі.
            // Саме тут живе перевірка токена для `api/`, тож `return #{ error: ... }`
            // має стати JSON так само, як у самому маршруті.
            return Ok((body_from(&returned, kind), state_now));
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
        body_from(&returned, kind)
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

    // `api/` віддає рівно те, що повернув скрипт: ні layout, ні піднятих
    // стилів. Дописаний `<style>` зробив би відповідь невалідним JSON, і
    // клієнт побачив би помилку розбору замість даних.
    if kind.is_api() {
        state_now = response.take();
        return Ok((page_html, state_now));
    }

    // Фрагмент із `partials/` не загортається в layout ніколи: він для того й
    // існує, щоб приїхати в уже відкриту сторінку.
    if is_htmx || kind == RouteKind::Partial {
        state_now = response.take();
        // Асети їдуть разом із фрагментом: стиль позначений хешем, скрипт
        // загорнутий у перевірку реєстру, тож повторно не виконається.
        let mut body = assets.append_to(page_html);
        // `<title>` живе в layout, а layout на цьому шляху взагалі не
        // рендериться — тому заголовок вкладки застигає на тому, що показала
        // перша сторінка. htmx сам оновлює `document.title`, якщо десь у тексті
        // відповіді є тег `<title>`, байдуже, усередині цілі свопу чи ні; лишається
        // додати його самим. Лише для сторінки (не партіала) і лише коли її
        // скрипт справді поставив `page.title` — інакше довелось би вгадувати
        // дефолт, який кожен проєкт задає по-своєму в своєму layout.
        if is_htmx && kind == RouteKind::Page {
            if let Some(title) = page_title(&globals) {
                body = format!("<title>{}</title>{body}", html_escape(&title));
            }
        }
        return Ok((body, state_now));
    }

    // Сторінка може попросити інший layout або відмовитись від нього зовсім:
    // `page.layout = "admin"` / `page.layout = false` (SYNTAX 6.2).
    let layout_path = match chosen_layout(&globals) {
        Layout::None => {
            state_now = response.take();
            return Ok((page_html, state_now));
        }
        Layout::Named(name) => state.config.layout_named(&name),
        Layout::Default => state.config.layout_path(),
    };
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
    //
    // Крім boosted-переходу: htmx візьме з відповіді лише `<body>`, і стилі з
    // `<head>` до сторінки не доїхали б — компонент, якого не було на
    // попередній сторінці, лишився б без стилю. Тому тут асети їдуть разом зі
    // сторінкою, як у фрагмента.
    let (page_html, head_assets, body_assets) = if full_swap {
        (assets.append_to(page_html), String::new(), String::new())
    } else {
        (page_html, assets.head(), assets.scripts())
    };
    let css = collect_head(state.config.files.as_ref(), &state.config.public_dir());
    let client = client_scripts(&state.config);
    // Layout без `<rhaix:head/>` (писали й так) лишився б без htmx — тоді
    // ядро йде туди, де воно стояло раніше, у `<rhaix:scripts/>`. Там воно
    // переживає boosted-своп лише завдяки охороні від повторного запуску.
    let (head, scripts) = if layout.source().text().contains("<rhaix:head") {
        (format!("{css}{head_assets}\n  {client}"), body_assets)
    } else {
        (
            format!("{css}{head_assets}"),
            format!("{client}\n  {body_assets}"),
        )
    };
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

/// Скрипти для `<head>`: реєстр, htmx, ядро, UI, далі `public/**.js` — саме
/// те, що в Node-RED-стартері доводилось вписувати в `index.html` руками.
///
/// Усе це — у `<head>`, а не в кінці `<body>`: htmx свопить лише `<body>`, і
/// скрипти звідти на boosted-переході виконувались би знову (до 1.2.5 так і
/// було — кожен такий перехід додавав ще один слухач тостів).
///
/// Ядро й UI — синхронно, до `<body>`: скрипти компонентів у `<body>`
/// виконуються під час розбору й мають застати і реєстр, і htmx. Скрипти
/// проєкту — `defer`: вони виконуються один раз, коли `<body>` уже є, тож
/// звичний `document.body.addEventListener(...)` на верхньому рівні працює.
fn client_scripts(config: &Config) -> String {
    let mut scripts = list_assets(config.files.as_ref(), &config.public_dir(), "js");
    // Розширення htmx (`htmx-ext-*.js`) — першими серед проєктних.
    scripts.sort_by_key(|src| !src.contains("htmx"));
    let own_ui = scripts.iter().any(|src| src == UI_OVERRIDE);

    let mut out = client::core_tags(own_ui);
    out.extend(
        scripts
            .into_iter()
            .filter(|src| src != UI_OVERRIDE)
            .map(|src| format!("<script src=\"{src}\" defer></script>")),
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

/// Який layout попросила сторінка.
enum Layout {
    Default,
    Named(String),
    None,
}

/// Прочитати `page.title`, якщо скрипт сторінки його поставив.
///
/// `None`, а не порожній рядок за замовчуванням: сторінка, що не чіпала
/// `page.title` узагалі, не повинна скидати вкладці заголовок на щось своє —
/// довший заголовок, поставлений раніше (наприклад, layout-ом при першому
/// повному завантаженні), має право лишитись.
fn page_title(globals: &Globals) -> Option<String> {
    let page = globals.get("page")?;
    let map = page.read_lock::<Map>()?;
    let value = map.get("title")?;
    if value.is_unit() {
        return None;
    }
    Some(display(value))
}

/// Прочитати `page.layout`, виставлений скриптом сторінки.
fn chosen_layout(globals: &Globals) -> Layout {
    let Some(page) = globals.get("page") else {
        return Layout::Default;
    };
    let Some(map) = page.read_lock::<Map>() else {
        return Layout::Default;
    };
    let Some(value) = map.get("layout") else {
        return Layout::Default;
    };

    if let Ok(flag) = value.as_bool() {
        // `page.layout = false` — сторінка сама собі документ.
        return if flag { Layout::Default } else { Layout::None };
    }
    let name = display(value);
    let safe = !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
    if !safe {
        tracing::warn!("page.layout = `{name}` — дозволені лише літери, цифри, `-` і `_`");
        return Layout::Default;
    }
    Layout::Named(name)
}

/// Прочитати `locales/*.toml` через ті самі `Files`, що й шаблони.
///
/// Ім'я файлу і є кодом мови: `locales/uk.toml` → `uk`. Порожня тека —
/// порожній каталог, і `t("ключ")` просто повертає ключ.
fn load_catalog(config: &Config) -> Catalog {
    let mut catalog = Catalog::new();
    for path in config.files.list(&config.root.join("locales"), "toml") {
        let Some(locale) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let Some(text) = config.files.read_text(&path) else {
            continue;
        };
        catalog.insert(locale, rhaix_script::parse_catalog_file(&text));
    }
    catalog
}

/// Об'єкти, які бачить кожен файл рендеру — і сторінка, і layout, і компоненти.
///
/// Компонент не успадковує scope батька (SYNTAX 5.3), тому глобальні об'єкти
/// передаються окремо — інакше в компоненті не було б ні `req`, ні `page`.
#[allow(clippy::too_many_arguments)]
fn globals_for(
    response: &ScriptResponse,
    state: &ScriptState,
    live: &Live,
    database: &Database,
    http: &Http,
    mail: &Mail,
    i18n: &I18n,
    session: &Session,
    csrf: &Csrf,
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
        .set("live", Dynamic::from(live.clone()))
        .set("db", Dynamic::from(database.clone()))
        .set("http", Dynamic::from(http.clone()))
        .set("mail", Dynamic::from(mail.clone()))
        .set("i18n", Dynamic::from(i18n.clone()))
        .set("session", Dynamic::from(session.clone()))
        .set("csrf", Dynamic::from(csrf.clone()))
        .set("log", Dynamic::from(Log { source }))
        .set("page", page);

    // Розмітка бере токен через замикання, а не через готовий рядок: форма в
    // компоненті, який так і не відрендерився, не має створювати сесію.
    let minter = csrf.clone();
    globals.with_csrf(Arc::new(move || minter.token()));
    globals
}

enum PageError {
    Io {
        file: PathBuf,
        message: String,
    },
    Template {
        diagnostic: String,
    },
    /// Запит, що змінює дані, без дійсного CSRF-токена.
    Forbidden {
        path: String,
    },
}

impl PageError {
    /// Відповідь у форматі, якого чекає саме цей маршрут.
    ///
    /// Клієнт, що отримує JSON на успіх і HTML на помилку, — непридатний:
    /// розбір падає рівно там, де потрібне пояснення. Тому `api/` віддає
    /// помилку так само машинно.
    fn respond(self, kind: RouteKind) -> Response {
        if !kind.is_api() {
            return self.into_response();
        }
        let (status, message) = match self {
            PageError::Io { file, message } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("не вдалося прочитати {}: {message}", file.display()),
            ),
            PageError::Template { diagnostic } => (StatusCode::INTERNAL_SERVER_ERROR, diagnostic),
            // До `api/` CSRF не застосовується, тож сюди можна потрапити лише
            // через `partials`/`pages`; лишаємо гілку заради повноти.
            PageError::Forbidden { path } => {
                (StatusCode::FORBIDDEN, format!("{path}: доступ заборонено"))
            }
        };
        tracing::error!("{message}");
        (status, json_body(&message)).into_response()
    }
}

/// Тіло помилки для `api/`: `{"error": "..."}` із правильним типом вмісту.
fn json_body(message: &str) -> impl IntoResponse {
    let mut map = Map::new();
    map.insert("error".into(), Dynamic::from(message.to_owned()));
    (
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        rhaix_script::json_encode(&Dynamic::from_map(map)),
    )
}

impl IntoResponse for PageError {
    fn into_response(self) -> Response {
        // Людина має бачити свій файл і свій рядок, а не стек Rust: сторінка
        // помилки показує діагностику в координатах `.rhx`.
        let (status, body) = match self {
            PageError::Io { file, message } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "error: не вдалося прочитати {}\n  = {message}",
                    file.display()
                ),
            ),
            PageError::Template { diagnostic } => (StatusCode::INTERNAL_SERVER_ERROR, diagnostic),
            // Найчастіша причина — не помилка розробника, а протермінована
            // вкладка: людина відкрила форму вчора, сесія закінчилась, токен
            // уже не той. Тому текст для людини, а не діагностика.
            PageError::Forbidden { path } => {
                tracing::warn!("{path}: запит без дійсного CSRF-токена відхилено");
                return (
                    StatusCode::FORBIDDEN,
                    [
                        (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                        // htmx покаже це у своїй цілі, тому текст має сенс сам по собі.
                        (header::CACHE_CONTROL, "no-store"),
                    ],
                    "<p>Термін дії форми минув. Оновіть сторінку й спробуйте ще раз.</p>",
                )
                    .into_response();
            }
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
            // Спільні скрипти підключаються в рушій один раз при старті, і
            // очищення кешу шаблонів їх не оновлює. Мовчати про це не можна:
            // людина правила б файл і не розуміла, чому нічого не змінюється.
            if event.paths.iter().any(|path| is_shared_script(path)) {
                tracing::warn!(
                    "змінено scripts/*.rhai — перезапустіть `rhaix dev`,                      спільні функції підключаються один раз при старті"
                );
            }
            tracing::info!("зміни підхоплено");
        }
    });
}

/// Чи варто реагувати на цей файл.
/// Файл зі спільними функціями проєкту.
fn is_shared_script(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("rhai")
}

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
            RouteKind::Api => "api     ",
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
    for script in config.files.list(&config.root.join("scripts"), "rhai") {
        println!(
            "  скрипт : {} — функції доступні скрізь",
            display_path(&config.root, &script)
        );
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
    // `ConnectInfo` — щоб `req.ip` знав адресу з'єднання.
    let app = router.into_make_service_with_connect_info::<SocketAddr>();
    let shutdown = || async {
        let _ = tokio::signal::ctrl_c().await;
    };

    // `localhost` на Windows (і не лише) спершу означає IPv6 `::1`. Сервер
    // лише на `127.0.0.1` там не відповідає, і клієнт чекає ~200 мс, перш ніж
    // спробувати IPv4 — на кожне нове з'єднання. Знайдено на CRM, де кожна
    // сторінка «відповідала» 0,2 с, а насправді — 5 мс. Тож на loopback
    // слухаємо обидві адреси; якщо IPv6 немає, просто лишаємось на IPv4.
    let v6 = match config.addr {
        SocketAddr::V4(v4) if v4.ip().is_loopback() && v4.port() != 0 => {
            tokio::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, v4.port()))
                .await
                .ok()
        }
        _ => None,
    };
    match v6 {
        Some(v6) => {
            tokio::try_join!(
                axum::serve(listener, app.clone()).with_graceful_shutdown(shutdown()),
                axum::serve(v6, app).with_graceful_shutdown(shutdown()),
            )?;
        }
        None => {
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown())
                .await?
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_names_cannot_point_anywhere_and_survive_any_browser() {
        let value = content_disposition("../звіт/\"x\".csv");
        // Роздільники шляху й лапки — `_`; не-ASCII у запасному імені — теж.
        assert!(value.contains("filename=\".._______x_.csv\""), "{value}");
        assert!(!value.contains('/'), "{value}");
        assert!(value.is_ascii(), "{value}");
        assert!(value.contains("filename*=UTF-8''.._%D0%B7"), "{value}");
    }

    #[test]
    fn download_types_follow_the_extension() {
        assert_eq!(content_type_for("a.csv", true), "text/csv; charset=utf-8");
        assert_eq!(
            content_type_for("a.JSON", true),
            "application/json; charset=utf-8"
        );
        assert_eq!(content_type_for("a.pdf", false), "application/pdf");
        assert_eq!(content_type_for("noext", false), "application/octet-stream");
        assert_eq!(content_type_for("noext", true), "text/plain; charset=utf-8");
    }

    #[test]
    fn client_ip_ignores_forwarded_headers_unless_told_to_trust_a_proxy() {
        let peer = Some("10.0.0.5".parse().unwrap());
        let mut headers = std::collections::BTreeMap::new();
        headers.insert(
            "x-forwarded-for".to_owned(),
            "6.6.6.6, 203.0.113.7".to_owned(),
        );

        // Без проксі заголовок підробляє будь-хто — не віримо.
        assert_eq!(client_ip(peer, &headers, false), "10.0.0.5");
        // За проксі — останній запис: його дописав наш проксі, а `6.6.6.6`
        // прийшло від клієнта.
        assert_eq!(client_ip(peer, &headers, true), "203.0.113.7");

        // Сміття в заголовку — назад до адреси з'єднання.
        headers.insert("x-forwarded-for".to_owned(), "не адреса".to_owned());
        assert_eq!(client_ip(peer, &headers, true), "10.0.0.5");

        headers.clear();
        headers.insert("x-real-ip".to_owned(), "198.51.100.1".to_owned());
        assert_eq!(client_ip(peer, &headers, true), "198.51.100.1");
        assert_eq!(client_ip(None, &headers, false), "");
    }

    #[test]
    fn catalog_loads_every_locale_file() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixture");
        let config = Config::load_for_check(root).expect("конфіг фікстури");
        let catalog = load_catalog(&config);
        let mut keys: Vec<&str> = catalog.keys().map(|k| k.as_str()).collect();
        keys.sort();
        assert_eq!(keys, vec!["en", "uk"], "каталог: {catalog:?}");
    }

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
