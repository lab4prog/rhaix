//! Перевірка схеми URL — одне правило на весь фреймворк.
//!
//! Ним користуються двоє: шаблонізатор (атрибути `href`/`src`/…) і `markdown()`
//! (посилання та картинки з тексту користувача). Тримати дві копії такого
//! правила — найкоротший шлях до дірки в одній із них.

/// Схеми, які дозволено лишати в посиланні.
const ALLOWED_SCHEMES: [&str; 5] = ["http", "https", "mailto", "tel", "ftp"];

/// Перевірити посилання. Заборонена схема замінюється на `#`.
///
/// Відносні шляхи, якорі та query проходять як є — у них схеми немає.
pub fn sanitize_url(value: &str) -> &str {
    let trimmed = value.trim_start_matches(|ch: char| ch.is_whitespace() || ch.is_control());

    let scheme_end = match trimmed.find([':', '/', '?', '#']) {
        Some(index) if trimmed.as_bytes()[index] == b':' => index,
        _ => return value, // схеми немає — відносний шлях
    };

    let scheme = trimmed[..scheme_end].to_ascii_lowercase();
    if ALLOWED_SCHEMES.contains(&scheme.as_str()) {
        return value;
    }
    // data: лишаємо тільки для картинок — усе інше вміє виконувати скрипт.
    if scheme == "data"
        && trimmed[scheme_end + 1..]
            .to_ascii_lowercase()
            .starts_with("image/")
    {
        return value;
    }
    "#"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dangerous_schemes_become_hash() {
        assert_eq!(sanitize_url("javascript:alert(1)"), "#");
        assert_eq!(sanitize_url("  JavaScript:alert(1)"), "#");
        assert_eq!(sanitize_url("data:text/html,<script>"), "#");
        assert_eq!(sanitize_url("vbscript:msgbox"), "#");
    }

    #[test]
    fn safe_links_pass_through() {
        assert_eq!(sanitize_url("/todo/1"), "/todo/1");
        assert_eq!(sanitize_url("https://example.com"), "https://example.com");
        assert_eq!(sanitize_url("mailto:a@b.co"), "mailto:a@b.co");
        assert_eq!(sanitize_url("#anchor"), "#anchor");
        assert_eq!(sanitize_url("data:image/png;base64,AAA"), "data:image/png;base64,AAA");
    }
}
