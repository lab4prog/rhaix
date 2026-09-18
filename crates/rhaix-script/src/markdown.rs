//! `markdown(текст)` — CommonMark у HTML, **безпечний за побудовою**.
//!
//! Функція повертає `Html`, тобто вивід не екранується. Для фреймворку, який
//! обіцяє «безпечно за замовчуванням», це небезпечне місце: текст найчастіше
//! приходить із поля форми чи бази, тобто від користувача.
//!
//! Тому тут **два запобіжники**, і вони не налаштовуються:
//!
//! 1. **Сирий HTML не проходить.** CommonMark дозволяє вставляти HTML прямо в
//!    текст; `<img src=x onerror=alert(1)>` пройшов би наскрізь. Ми такі шматки
//!    не виводимо як розмітку, а екрануємо як текст — тож у HTML потрапляють
//!    лише ті теги, які згенерував сам markdown.
//! 2. **Схема посилань перевіряється** тим самим правилом, що й атрибути
//!    шаблонізатора (`urls::sanitize_url`): `[клац](javascript:alert(1))`
//!    перетворюється на `#`.
//!
//! Через це санітайзер HTML (`ammonia` і весь html5ever) не потрібен:
//! небезпечному просто нема звідки взятися.
//!
//! ```rhai
//! <div>{{ markdown(post.body) }}</div>
//! ```

use rhai::Engine;

use crate::stdlib::Html;

/// Перетворити CommonMark на HTML.
#[cfg(feature = "markdown")]
pub fn markdown(text: &str) -> Html {
    use pulldown_cmark::{html, Event, Options, Parser, Tag};

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);

    let events = Parser::new_ext(text, options).map(|event| match event {
        // Сирий HTML із тексту — у вивід як **текст**, а не як розмітка.
        Event::Html(raw) => Event::Text(raw),
        Event::InlineHtml(raw) => Event::Text(raw),
        // Посилання й картинки: схема за тим самим правилом, що й у шаблоні.
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: crate::urls::sanitize_url(&dest_url).to_owned().into(),
            title,
            id,
        }),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Image {
            link_type,
            dest_url: crate::urls::sanitize_url(&dest_url).to_owned().into(),
            title,
            id,
        }),
        other => other,
    });

    let mut out = String::with_capacity(text.len() + text.len() / 2);
    html::push_html(&mut out, events);
    Html(out)
}

/// Без feature `markdown` функція є, але чесно каже, чого бракує — це краще за
/// «невідома функція `markdown`», яка нічого не пояснює.
#[cfg(not(feature = "markdown"))]
pub fn markdown(_text: &str) -> Result<Html, Box<rhai::EvalAltResult>> {
    Err("markdown() не увімкнено в цій збірці; додайте feature `markdown` \
         (у проді це робить `rhaix build`, коли бачить markdown у проєкті)"
        .into())
}

pub fn register_markdown(engine: &mut Engine) {
    engine.register_fn("markdown", markdown);
}

#[cfg(all(test, feature = "markdown"))]
mod tests {
    use super::*;

    fn render(text: &str) -> String {
        markdown(text).0
    }

    #[test]
    fn basic_markdown_becomes_html() {
        assert!(render("# Заголовок").contains("<h1>Заголовок</h1>"));
        assert!(render("*курсив*").contains("<em>курсив</em>"));
        assert!(render("- один\n- два").contains("<li>один</li>"));
    }

    #[test]
    fn raw_html_is_escaped_not_passed_through() {
        // Найважливіший тест: текст із полем форми не має внести скрипт.
        let out = render("<img src=x onerror=alert(1)>");
        assert!(!out.contains("<img"), "{out}");
        assert!(out.contains("&lt;img"), "{out}");

        let out = render("Привіт <script>alert(1)</script>");
        assert!(!out.contains("<script>"), "{out}");
        assert!(out.contains("&lt;script&gt;"), "{out}");
    }

    #[test]
    fn javascript_links_are_neutralised() {
        let out = render("[клац](javascript:alert(1))");
        assert!(!out.contains("javascript:"), "{out}");
        assert!(out.contains("href=\"#\""), "{out}");

        // А звичайні посилання лишаються.
        let out = render("[клац](https://example.com)");
        assert!(out.contains("href=\"https://example.com\""), "{out}");
    }

    #[test]
    fn image_sources_are_checked_too() {
        let out = render("![alt](javascript:alert(1))");
        assert!(!out.contains("javascript:"), "{out}");
    }
}
