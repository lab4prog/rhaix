//! Екранування за контекстом (SYNTAX 2.5).
//!
//! Одного HTML-екранування недостатньо: усередині `href` воно не рятує від
//! `javascript:`, а всередині `<script>` взагалі не діє. Тому контекст
//! визначається при компіляції, а тут лежать самі правила.

/// Контекст, у якому опиняється значення.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    /// Текст у розмітці.
    Text,
    /// Значення звичайного атрибута.
    Attribute,
    /// Значення атрибута-посилання: додатково перевіряється схема.
    Url,
    /// Вміст `<script>`: дозволено лише `json(...)`, тому екранування вже зроблене.
    Script,
}

/// HTML-екранування для тексту й атрибутів.
///
/// Копіюємо шматками між небезпечними символами: у типовому тексті екранувати
/// нічого, і тоді це один memcpy замість посимвольного запису.
pub fn escape_html(text: &str, out: &mut String) {
    let bytes = text.as_bytes();
    let mut last = 0usize;
    for (index, byte) in bytes.iter().enumerate() {
        // усі підміни — ASCII, тому індекси завжди стоять на межі символу
        let replacement = match byte {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' => "&quot;",
            b'\'' => "&#39;",
            _ => continue,
        };
        out.push_str(&text[last..index]);
        out.push_str(replacement);
        last = index + 1;
    }
    out.push_str(&text[last..]);
}

/// Атрибути, значення яких є посиланням.
pub fn is_url_attribute(name: &str) -> bool {
    matches!(
        name,
        "href" | "src" | "action" | "formaction" | "xlink:href" | "poster" | "data" | "ping"
    )
}

/// `onclick`, `onerror`, … — сюди не можна пускати значення з виразів.
///
/// Правило свідомо широке: будь-який атрибут, що починається на `on`, вважається
/// обробником події. Стандартного HTML-атрибута з такою назвою, який не є
/// обробником, не існує, а помилитись тут дорожче, ніж перестрахуватись.
pub fn is_event_attribute(name: &str) -> bool {
    let rest = match name.strip_prefix("on") {
        Some(rest) => rest,
        None => return false,
    };
    !rest.is_empty() && rest.chars().all(|ch| ch.is_ascii_alphabetic() || ch == '-')
}

/// Перевірити посилання. Заборонена схема замінюється на `#`.
///
/// Саме правило живе в `rhaix-script` (`urls.rs`): ним користується ще й
/// `markdown()`, а дві копії такої перевірки — найкоротший шлях до дірки в
/// одній із них.
pub use rhaix_script::sanitize_url;

#[cfg(test)]
mod tests {
    use super::*;

    fn esc(text: &str) -> String {
        let mut out = String::new();
        escape_html(text, &mut out);
        out
    }

    #[test]
    fn escapes_everything_that_can_break_out() {
        assert_eq!(esc("<b>&\"'"), "&lt;b&gt;&amp;&quot;&#39;");
        assert_eq!(esc("звичайний текст"), "звичайний текст");
    }

    #[test]
    fn recognises_url_and_event_attributes() {
        assert!(is_url_attribute("href"));
        assert!(is_url_attribute("formaction"));
        assert!(!is_url_attribute("hreflang"));

        assert!(is_event_attribute("onclick"));
        assert!(is_event_attribute("onerror"));
        assert!(
            is_event_attribute("once"),
            "перестраховуємось: `on*` завжди обробник"
        );
        assert!(!is_event_attribute("on"));
        assert!(
            !is_event_attribute("hx-on:click"),
            "htmx-атрибути починаються з `hx-`"
        );
    }

    #[test]
    fn relative_urls_pass_through() {
        assert_eq!(sanitize_url("/orders?page=2"), "/orders?page=2");
        assert_eq!(sanitize_url("#anchor"), "#anchor");
        assert_eq!(sanitize_url("orders/42"), "orders/42");
        assert_eq!(sanitize_url("https://example.com"), "https://example.com");
        assert_eq!(sanitize_url("mailto:a@b.c"), "mailto:a@b.c");
    }

    #[test]
    fn dangerous_schemes_become_hash() {
        assert_eq!(sanitize_url("javascript:alert(1)"), "#");
        assert_eq!(sanitize_url("JaVaScRiPt:alert(1)"), "#");
        assert_eq!(sanitize_url("  \n javascript:alert(1)"), "#");
        assert_eq!(sanitize_url("vbscript:msgbox"), "#");
        assert_eq!(sanitize_url("data:text/html,<script>"), "#");
    }

    #[test]
    fn data_image_is_allowed() {
        assert_eq!(
            sanitize_url("data:image/png;base64,iVBOR"),
            "data:image/png;base64,iVBOR"
        );
    }
}
