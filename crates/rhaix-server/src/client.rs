//! Клієнтська частина, яку ядро віддає саме.
//!
//! Два шари, і межа між ними — навмисна:
//!
//! - **ядро** (`/_rhaix/rhaix.js`, `client/rhaix.js`) — протокол між сервером і
//!   htmx: реєстр асетів компонентів, своп відповідей із кодом ≠ 2xx,
//!   дедуплікація стилів. Замінювати не треба й не можна.
//! - **UI** (`/_rhaix/ui.js`, `client/ui.js`) — тости й модальні вікна. Це
//!   лише типова реалізація: проєкт перевизначає окремі функції або цілком
//!   забирає файл собі (`rhaix eject ui` → `public/rhaix-ui.js`), і тоді ядро
//!   підключає його замість вбудованого.
//!
//! Обидва — у `<head>`, синхронно. htmx свопить лише `<body>`, тож на
//! boosted-переходах вони не виконуються вдруге (до 1.2.5 стояли в кінці
//! `<body>` і перевиконувались: тости множились із кожним таким переходом).

/// Шлях ядра клієнта.
pub const CLIENT_ROUTE: &str = "/_rhaix/rhaix.js";

/// Шлях вбудованого UI (тости, модалки).
pub const UI_ROUTE: &str = "/_rhaix/ui.js";

/// Шлях htmx.
pub const HTMX_ROUTE: &str = "/_rhaix/htmx.js";

/// Версія вшитого htmx. Іде в адресу скрипта як `?v=`: файл кешується на рік
/// (`immutable`), і без цього оновлення htmx у новому релізі rhaix не дійшло б
/// до браузерів, які вже мають стару копію.
pub const HTMX_VERSION: &str = "2.0.7";

/// Файл у `public/`, який заміняє вбудований UI. Шлях — як його бачить браузер.
pub const UI_OVERRIDE: &str = "/rhaix-ui.js";

/// htmx, вшитий у бінарник.
///
/// Інакше обіцянка «деплой — це копіювання одного файлу» була б неправдою:
/// сторінка не працювала б без інтернету й чужого CDN. 51 КБ у бінарнику —
/// чесна ціна за застосунок, який справді самодостатній.
///
/// htmx 2.0.7, ліцензія Zero-Clause BSD (дозволяє використання й поширення
/// без умов). Джерело: https://htmx.org
pub const HTMX_JS: &str = include_str!("../vendor/htmx.min.js");

/// Ядро клієнта.
pub const CLIENT_JS: &str = include_str!("../client/rhaix.js");

/// Типовий UI: тости й модалки. Той самий текст кладе `rhaix eject ui`.
pub const UI_JS: &str = include_str!("../client/ui.js");

/// Реєстр асетів — інлайн, першим у `<head>`.
///
/// Скрипти компонентів стоять у `<body>` і виконуються під час розбору
/// сторінки; реєстр мусить існувати раніше за них. Окремим файлом він не
/// встиг би: той міг би ще завантажуватись.
pub const REGISTRY_JS: &str = "window.__rhaix=window.__rhaix||{};if(!window.__rhaix.seen){const s=new Set();window.__rhaix.seen=h=>s.has(h)||(s.add(h),false)}";

/// Теги для `<head>`: реєстр, htmx, ядро, UI (вбудований або проєктний).
///
/// `own_ui` — чи є в проєкті `public/rhaix-ui.js`.
pub fn core_tags(own_ui: bool) -> Vec<String> {
    let ui = if own_ui { UI_OVERRIDE } else { UI_ROUTE };
    vec![
        format!("<script>{REGISTRY_JS}</script>"),
        format!("<script src=\"{HTMX_ROUTE}?v={HTMX_VERSION}\"></script>"),
        format!("<script src=\"{CLIENT_ROUTE}\"></script>"),
        format!("<script src=\"{ui}\"></script>"),
    ]
}

/// `<style>` компонента у вигляді тега з міткою.
pub fn style_tag(hash: &str, body: &str) -> String {
    format!("<style data-rhx=\"{hash}\">{body}</style>")
}

/// `<script>` компонента, загорнутий у перевірку реєстру.
///
/// Саме ця обгортка робить дедуплікацію для фрагментів: htmx виконує скрипти
/// у вставленому HTML, тому без неї код компонента виконувався б знову на
/// кожному свопі.
pub fn script_tag(hash: &str, body: &str) -> String {
    format!("<script data-rhx=\"{hash}\">if(!window.__rhaix||!window.__rhaix.seen(\"{hash}\")){{{body}}}</script>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_is_wrapped_in_the_registry_check() {
        let tag = script_tag("abc123", "console.log(1);");
        assert!(tag.contains("data-rhx=\"abc123\""), "{tag}");
        assert!(tag.contains("__rhaix.seen(\"abc123\")"), "{tag}");
        assert!(tag.contains("console.log(1);"), "{tag}");
    }

    #[test]
    fn the_core_has_no_user_interface() {
        // Межа між шарами: у ядрі немає ні тостів, ні модалок — інакше їх не
        // можна було б замінити, не лагодячи фреймворк.
        assert!(!CLIENT_JS.contains("showToast"));
        assert!(!CLIENT_JS.contains("showModal"));
        assert!(!CLIENT_JS.contains("createElement"));
    }

    #[test]
    fn error_status_responses_still_swap_but_204_does_not() {
        // За замовчуванням htmx свопить лише 2xx і мовчки викидає решту —
        // без цього `res.status(422)` із помилками під полями чи `403` з
        // поясненням ніколи не з'являються на екрані.
        assert!(CLIENT_JS.contains("htmx.config.responseHandling"));
        assert!(CLIENT_JS.contains(r#"{ code: "[45]..", swap: true, error: true }"#));
        // 204 — без тіла: своп стер би ціль.
        assert!(CLIENT_JS.contains(r#"{ code: "204", swap: false }"#));
        // 422 — штатна валідація, не збій; і правило для нього стоїть раніше
        // за загальне [45].., бо htmx бере перше, що збіглося.
        let exact = CLIENT_JS
            .find(r#"{ code: "422", swap: true }"#)
            .expect("окреме правило для 422");
        let general = CLIENT_JS.find(r#"{ code: "[45].."#).expect("загальне");
        assert!(exact < general);
    }

    #[test]
    fn both_layers_are_idempotent() {
        // Якщо файл виконався вдруге (layout без <rhaix:head/>, boosted-своп),
        // слухачі не мають реєструватись повторно — інакше кожен тост двічі.
        assert!(CLIENT_JS.contains("if (rhaix.coreLoaded) return;"));
        assert!(UI_JS.contains("if (rhaix.uiLoaded) return;"));
    }

    #[test]
    fn toast_rendering_is_an_overridable_hook() {
        // `||` — не перезаписати те, що вже поклав проєкт.
        assert!(UI_JS.contains("rhaix.toast = rhaix.toast || function"));
        // Кілька hx.toast(...) за запит приходять одним detail з `items`.
        assert!(UI_JS.contains("detail.items"));
    }

    #[test]
    fn toasts_carried_in_the_page_are_shown_once() {
        // Сервер кладе їх у `<script data-rhx-toasts>` (flash і звичайні GET).
        assert!(UI_JS.contains("script[data-rhx-toasts]"));
        // Прибирається після показу: boosted-перехід назад їх не повторить.
        assert!(UI_JS.contains("carrier.remove()"));
    }

    #[test]
    fn dialog_elements_are_opened_as_native_modals() {
        assert!(
            UI_JS.contains("htmx:load"),
            "єдина подія на всі шляхи появи"
        );
        assert!(UI_JS.contains("showModal()"));
        assert!(
            UI_JS.contains("data-plain"),
            "має бути шлях відмовитись від автомодалу"
        );
    }

    #[test]
    fn head_tags_put_the_registry_first_and_swap_in_a_project_ui() {
        let builtin = core_tags(false);
        assert!(builtin[0].contains("window.__rhaix.seen"), "{builtin:?}");
        assert!(builtin[1].contains("htmx.js?v=2.0.7"), "{builtin:?}");
        assert!(builtin[3].contains(UI_ROUTE), "{builtin:?}");

        let own = core_tags(true);
        assert!(own[3].contains(UI_OVERRIDE), "{own:?}");
        assert!(!own.iter().any(|tag| tag.contains(UI_ROUTE)), "{own:?}");
    }
}
