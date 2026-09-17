//! HTTP-шар rhaix: маршрути з файлової структури, layout і правило фрагмента.
//!
//! Сервер знаходить сторінку, рендерить її шаблонізатором і віддає — з layout
//! при звичайному заході й без нього при HTMX-запиті.
//!
//! Обсяг M1: `{{ }}`, директиви й екранування працюють, але дані поки
//! підставляє сам сервер (`stub_scope`) — виконання frontmatter приїде в M2,
//! компоненти — в M3, кеш шаблонів — в M6.

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use rhai::{Dynamic, Engine, Map, Scope};
use rhaix_parser::Source;
use rhaix_script::{engine as build_engine, Limits};
use rhaix_template::{Slots, Template};
use tower_http::services::ServeDir;

/// Налаштування застосунку. У M6 сюди приїде `rhaix.toml`.
#[derive(Debug, Clone)]
pub struct Config {
    pub root: PathBuf,
    pub addr: SocketAddr,
}

impl Config {
    pub fn new(root: impl Into<PathBuf>, addr: SocketAddr) -> Self {
        Self {
            root: root.into(),
            addr,
        }
    }

    pub fn pages_dir(&self) -> PathBuf {
        self.root.join("pages")
    }

    pub fn public_dir(&self) -> PathBuf {
        self.root.join("public")
    }

    pub fn layout_path(&self) -> PathBuf {
        self.root.join("layouts").join("main.rhx")
    }
}

/// Сторінка, знайдена при скануванні `pages/`.
#[derive(Debug, Clone)]
pub struct PageRoute {
    /// Шлях у стилі axum: `/todo`, `/todo/{id}`, `/blog/{*rest}`.
    pub pattern: String,
    pub file: PathBuf,
}

#[derive(Clone)]
struct AppState {
    config: Config,
    /// Рушій спільний для всіх запитів: `sync`-збірка Rhai дозволяє тримати
    /// його в `Arc`, а на запит створюється лише `Scope`.
    engine: Arc<Engine>,
}

/// Зібрати застосунок: сторінки + статика з `public/`.
pub fn build(config: Config) -> anyhow::Result<(Router, Vec<PageRoute>)> {
    let routes = scan_pages(&config.pages_dir())?;
    let state = AppState {
        config: config.clone(),
        engine: Arc::new(build_engine(Limits::default())),
    };

    let mut router = Router::new();
    for route in &routes {
        let file = route.file.clone();
        router = router.route(
            &route.pattern,
            any(
                move |state: State<AppState>, uri: Uri, headers: HeaderMap| {
                    let file = file.clone();
                    async move { serve_page(state.0, uri, headers, file).await }
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

fn segment_pattern(name: &str) -> String {
    if let Some(inner) = name.strip_prefix("[...").and_then(|s| s.strip_suffix(']')) {
        return format!("{{*{inner}}}");
    }
    if let Some(inner) = name.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return format!("{{{inner}}}");
    }
    name.to_owned()
}

async fn serve_page(state: AppState, uri: Uri, headers: HeaderMap, file: PathBuf) -> Response {
    let is_htmx = headers.contains_key("hx-request");

    let html = match render_page(&state, &file, uri.path(), is_htmx) {
        Ok(html) => html,
        Err(err) => return err.into_response(),
    };

    let mut response = Response::new(Body::from(html));
    let out = response.headers_mut();
    out.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    // Vary самого по собі мало: деякі CDN його ігнорують і можуть віддати
    // фрагмент замість сторінки (RISKS 2.9), тому HTMX-відповіді не кешуємо.
    out.insert(header::VARY, HeaderValue::from_static("HX-Request"));
    if is_htmx {
        out.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
    }
    response
}

/// Відрендерити сторінку і, якщо треба, вкласти її в layout.
///
/// Правило фрагмента (SYNTAX 6.3): layout додається лише тоді, коли сторінку
/// відкривають напряму. Для HTMX-запиту віддається сама розмітка сторінки.
fn render_page(
    state: &AppState,
    file: &Path,
    path: &str,
    is_htmx: bool,
) -> Result<String, PageError> {
    let template = load_template(&state.engine, file, &state.config.root)?;
    let mut scope = stub_scope(path, is_htmx);

    let rendered = template
        .render(&state.engine, &mut scope, Slots::default())
        .map_err(|diagnostic| PageError::Template {
            diagnostic: template.describe(&diagnostic),
        })?;
    for warning in &rendered.warnings {
        tracing::warn!("{path}: {warning}");
    }

    if is_htmx {
        return Ok(rendered.html);
    }

    let layout_path = state.config.layout_path();
    if !layout_path.is_file() {
        return Ok(rendered.html);
    }

    let layout = load_template(&state.engine, &layout_path, &state.config.root)?;
    let head = collect_head(&state.config.public_dir());
    let scripts = collect_scripts(&state.config.public_dir());
    let mut layout_scope = stub_scope(path, is_htmx);
    let wrapped = layout
        .render(
            &state.engine,
            &mut layout_scope,
            Slots {
                slot: &rendered.html,
                head: &head,
                scripts: &scripts,
            },
        )
        .map_err(|diagnostic| PageError::Template {
            diagnostic: layout.describe(&diagnostic),
        })?;
    Ok(wrapped.html)
}

/// Прочитати й скомпілювати `.rhx`.
///
/// M6: тут з'явиться кеш `шлях + mtime → Arc<Template>`; поки що компіляція
/// на кожен запит — так dev-режим завжди показує свіжий файл.
fn load_template(engine: &Engine, file: &Path, root: &Path) -> Result<Template, PageError> {
    let raw = fs::read_to_string(file).map_err(|err| PageError::Io {
        file: file.to_path_buf(),
        message: err.to_string(),
    })?;
    // У діагностиці показуємо шлях так, як його бачить людина: відносно кореня
    // проєкту, а не UNC-шлях, який дає canonicalize на Windows.
    let source = Arc::new(Source::new(display_path(root, file), raw));
    Template::compile(source.clone(), engine).map_err(|diagnostic| PageError::Template {
        diagnostic: diagnostic.render(&source),
    })
}

/// Дані-заглушки замість frontmatter (M2).
///
/// Це єдине місце в M1, яке знає щось про вміст сторінок, і воно зникне
/// повністю, щойно frontmatter почне виконуватись.
fn stub_scope(path: &str, is_htmx: bool) -> Scope<'static> {
    let mut scope = Scope::new();

    let mut page = Map::new();
    page.insert("title".into(), Dynamic::from("rhaix"));
    scope.push_dynamic("page", Dynamic::from_map(page));
    scope.push("path", path.to_owned());
    scope.push("is_htmx", is_htmx);

    let todos: Vec<Dynamic> = [
        ("Купити молоко", true),
        ("Зробити домашку", false),
        ("Написати M2", false),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (title, done))| {
        let mut todo = Map::new();
        todo.insert("id".into(), Dynamic::from(index as i64 + 1));
        todo.insert("title".into(), Dynamic::from(title));
        todo.insert("done".into(), Dynamic::from(done));
        Dynamic::from_map(todo)
    })
    .collect();
    scope.push_dynamic("todos", Dynamic::from(todos));

    let features: Vec<Dynamic> = [
        "інтерполяція {{ }} з екрануванням за контекстом",
        "директиви @if / @else / @for / @class / @attr",
        "помилки з позицією у файлі .rhx",
    ]
    .into_iter()
    .map(Dynamic::from)
    .collect();
    scope.push_dynamic("features", Dynamic::from(features));

    // для сторінки про екранування
    scope.push("dangerous", "<script>alert(1)</script>");
    scope.push("markup", "<b>це справді жирний текст</b>");
    scope.push("evil", "javascript:alert(document.cookie)");

    scope
}

/// Усі `public/**.css` — у `<head>`.
fn collect_head(public: &Path) -> String {
    list_assets(public, "css")
        .into_iter()
        .map(|href| format!("<link rel=\"stylesheet\" href=\"{href}\">"))
        .collect::<Vec<_>>()
        .join("\n  ")
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
        .join("\n  ")
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

async fn not_found(_: Request<Body>) -> Response {
    (StatusCode::NOT_FOUND, "404").into_response()
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
        println!(
            "  маршрут: {:<22} {}",
            route.pattern,
            display_path(&config.root, &route.file)
        );
    }
    println!("  M1: `{{{{ }}}}` і директиви працюють; дані — заглушки, компоненти — з M3\n");

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
            },
            PageRoute {
                pattern: "/todo/new".into(),
                file: PathBuf::new(),
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
    fn scripts_put_htmx_first() {
        let mut scripts = ["/app.js".to_owned(), "/vendor/htmx.min.js".to_owned()];
        scripts.sort_by_key(|src| !src.contains("htmx"));
        assert_eq!(scripts[0], "/vendor/htmx.min.js");
    }
}
