//! `rhaix new` не має генерувати зламаний проєкт — ні для компілятора
//! (`rhaix check`), ні в браузері (заголовок вкладки, підсвітка меню).
//!
//! Той самий клас бага, що й у `examples/demo`/`examples/cookbook` (M15):
//! `rhaix-cli` не ділиться файлами з `examples/`, скелет — окрема копія
//! в `src/scaffold.rs`, тож фікс в одному місці не рятує інше самим фактом
//! свого існування. Цей тест — щоб таке розходження не повторилось мовчки.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rhaix_server::{build, check, Config};
use tower::ServiceExt;

/// Унікальна тека в системному temp: тести можуть іти паралельно, а PID у
/// цьому процесі той самий для всіх.
fn temp_project() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("rhaix-scaffold-test-{}-{n}", std::process::id()))
}

/// `rhaix new <path>` — справжній бінарник, не виклик функції: `rhaix-cli`
/// не має бібліотечної цілі, і саме так скелет побачить реальний користувач.
fn scaffold(path: &std::path::Path) {
    let exe = env!("CARGO_BIN_EXE_rhaix");
    let status = Command::new(exe)
        .arg("new")
        .arg(path)
        .status()
        .expect("`rhaix new` має запуститись");
    assert!(status.success(), "`rhaix new` завершився з помилкою");
}

fn get(path: &str) -> Request<Body> {
    Request::builder().uri(path).body(Body::empty()).unwrap()
}

fn htmx(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header("HX-Request", "true")
        .body(Body::empty())
        .unwrap()
}

async fn body_of(app: axum::Router, request: Request<Body>) -> (StatusCode, String) {
    let response = app.oneshot(request).await.expect("запит має оброблятись");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("тіло має читатись");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// Чи має посилання з таким `href` клас `active` у цьому HTML. Не парсер DOM —
/// достатньо знайти тег і зазирнути в нього до першого `>`.
fn link_is_active(html: &str, href: &str) -> bool {
    let needle = format!("href=\"{href}\"");
    let Some(start) = html.find(&needle) else {
        panic!("посилання {href} не знайдено: {html}");
    };
    let tag_start = html[..start].rfind('<').unwrap_or(0);
    let tag_end = html[start..]
        .find('>')
        .map(|i| start + i)
        .unwrap_or(html.len());
    html[tag_start..tag_end].contains("active")
}

#[test]
fn a_fresh_project_compiles_without_errors() {
    let root = temp_project();
    scaffold(&root);

    let config = Config::load_for_check(root.clone()).expect("конфіг скелета");
    let issues = check(&config);
    let _ = std::fs::remove_dir_all(&root);

    assert!(
        issues.is_empty(),
        "скелет `rhaix new` не компілюється:\n{}",
        issues
            .iter()
            .map(|issue| issue.rendered.clone())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[tokio::test]
async fn a_fresh_project_updates_the_title_and_the_active_nav_item_on_navigation() {
    let root = temp_project();
    scaffold(&root);

    let config = Config::load(root.clone(), Some(0)).expect("конфіг скелета");
    let (router, _) = build(config).expect("скелет має збиратись у застосунок");

    // Повне завантаження: `/` активний, `/todo` — ні.
    let (status, full) = body_of(router.clone(), get("/")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(full.contains("<!DOCTYPE html>"), "{full}");
    assert!(link_is_active(&full, "/"), "{full}");
    assert!(!link_is_active(&full, "/todo"), "{full}");

    // Перехід на `/todo` htmx-навігацією: рівно те, що не працювало до
    // виправлення — заголовок і підсвітка мають оновитись у ЦІЙ відповіді,
    // а не лишитись такими, якими їх намалював layout на першому завантаженні.
    let (status, fragment) = body_of(router, htmx("/todo")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        fragment.starts_with("<title>Справи</title>"),
        "заголовок вкладки не прийшов у фрагменті: {fragment}"
    );
    assert!(
        fragment.contains("hx-swap-oob=\"outerHTML:#nav\""),
        "оновлення меню не прийшло поза ціллю: {fragment}"
    );
    assert!(link_is_active(&fragment, "/todo"), "{fragment}");
    assert!(!link_is_active(&fragment, "/"), "{fragment}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_fresh_project_does_not_draw_toasts_a_second_time() {
    // До 1.2.5 скелет клав у public/app.js власний слухач showToast — поруч із
    // тим, що вже є у фреймворку, і кожен тост у новому проєкті малювався двічі.
    let root = temp_project();
    scaffold(&root);
    let app_js = std::fs::read_to_string(root.join("public/app.js")).expect("app.js");
    let _ = std::fs::remove_dir_all(&root);

    assert!(!app_js.contains("addEventListener"), "{app_js}");
}

#[tokio::test]
async fn eject_ui_hands_the_interface_to_the_project() {
    let root = temp_project();
    scaffold(&root);

    let status = Command::new(env!("CARGO_BIN_EXE_rhaix"))
        .args(["eject", "ui", "--root"])
        .arg(&root)
        .status()
        .expect("`rhaix eject ui` має запуститись");
    assert!(status.success());

    // Файл — рівно вбудований UI, і далі він належить проєкту.
    let owned = std::fs::read_to_string(root.join("public/rhaix-ui.js")).expect("rhaix-ui.js");
    assert_eq!(owned, rhaix_server::UI_JS);

    // Сторінка підключає файл проєкту ЗАМІСТЬ вбудованого, і лише один раз.
    let config = Config::load(root.clone(), Some(0)).expect("конфіг");
    let (router, _) = build(config).expect("застосунок");
    let (_, page) = body_of(router, get("/")).await;
    assert!(
        page.contains(r#"<script src="/rhaix-ui.js"></script>"#),
        "{page}"
    );
    assert!(!page.contains("/_rhaix/ui.js"), "{page}");
    assert_eq!(page.matches("rhaix-ui.js").count(), 1, "{page}");

    // Повторний eject без --force нічого не перезаписує.
    let again = Command::new(env!("CARGO_BIN_EXE_rhaix"))
        .args(["eject", "ui", "--root"])
        .arg(&root)
        .status()
        .expect("повторний запуск");
    assert!(
        !again.success(),
        "без --force файл проєкту не перезаписується"
    );

    let _ = std::fs::remove_dir_all(&root);
}
