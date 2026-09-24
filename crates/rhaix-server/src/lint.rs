//! Попередження `rhaix check` про розмітку, яка компілюється, але робить не те.
//!
//! Обидві пастки знайдено в справжньому застосунку (CRM на rhaix):
//!
//! - `<td>{company.name}</td>` — в атрибутах вираз пишеться `href={x}`, і рука
//!   за звичкою пише так само в тексті. Але в тексті одинарні дужки — просто
//!   символи (SYNTAX 2.4), і в таблиці клієнтів замість назв стояло
//!   буквальне `{company.name}`.
//! - `<option @class={#{"selected": s == x}}>` — `selected` тут стає **класом**,
//!   а не атрибутом, тож список після фільтра завжди показував перший пункт.
//!   Для атрибутів-прапорців є `@attr`.

/// Атрибути-прапорці: у `@class` вони нічого не вмикають.
const BOOLEAN_ATTRIBUTES: [&str; 9] = [
    "selected",
    "checked",
    "disabled",
    "readonly",
    "required",
    "hidden",
    "open",
    "multiple",
    "autofocus",
];

/// Одне попередження: зсув у тексті файлу, повідомлення, підказка.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub offset: usize,
    pub message: String,
    pub hint: String,
}

/// Де починається розмітка: після frontmatter, якщо він є.
fn markup_start(text: &str) -> usize {
    let Some(rest) = text.strip_prefix("---") else {
        return 0;
    };
    match rest.find("\n---") {
        Some(end) => {
            let after = 3 + end + 4;
            // До кінця рядка з закривальним `---`.
            text[after..]
                .find('\n')
                .map(|n| after + n + 1)
                .unwrap_or(text.len())
        }
        None => text.len(),
    }
}

/// Чи схоже вміст `{…}` на вираз, який хотіли вивести: `name`, `c.name`,
/// `row["x"].y`. Речення, CSS чи JSON сюди не підпадають.
fn looks_like_expression(inner: &str) -> bool {
    let inner = inner.trim();
    let Some(first) = inner.chars().next() else {
        return false;
    };
    if !(first.is_alphabetic() || first == '_') {
        return false;
    }
    inner.len() <= 60
        && inner
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '[' | ']' | '"' | '(' | ')'))
}

pub fn lint(text: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    let start = markup_start(text);
    let bytes = text.as_bytes();
    let mut i = start;

    while i < bytes.len() {
        let rest = &text[i..];

        // `{{ … }}` — справжня інтерполяція (і `{{! коментар }}`): пропустити.
        if rest.starts_with("{{") {
            i += rest.find("}}").map(|n| n + 2).unwrap_or(rest.len());
            continue;
        }
        // Вміст <style>, <script> і <rhaix:raw> — не розмітка rhaix.
        let lower = rest.get(..12).unwrap_or(rest).to_ascii_lowercase();
        let raw_end = if lower.starts_with("<style") {
            Some("</style>")
        } else if lower.starts_with("<script") {
            Some("</script>")
        } else if lower.starts_with("<rhaix:raw") {
            Some("</rhaix:raw>")
        } else {
            None
        };
        if let Some(end) = raw_end {
            let close = rest.to_ascii_lowercase().find(end);
            i += close.map(|n| n + end.len()).unwrap_or(rest.len());
            continue;
        }
        // Тег: вирази в атрибутах (`href={x}`) — законні. Перевіряємо лише
        // `@class` на атрибути-прапорці.
        if rest.starts_with('<') {
            let end = tag_end(rest);
            check_class(&rest[..end], i, &mut findings);
            i += end;
            continue;
        }
        if let Some(after) = rest.strip_prefix('{') {
            if let Some(close) = after.find(['}', '\n', '<']) {
                let inner = &after[..close];
                if after[close..].starts_with('}') && looks_like_expression(inner) {
                    let inner = inner.trim();
                    findings.push(Finding {
                        offset: i,
                        message: format!(
                            "`{{{inner}}}` у тексті виводиться буквально — одинарні дужки \
                             працюють лише в атрибутах"
                        ),
                        hint: format!("виведіть значення так: {{{{ {inner} }}}}"),
                    });
                }
            }
        }
        i += rest.chars().next().map(char::len_utf8).unwrap_or(1);
    }
    findings
}

/// Кінець тегу: `>` поза лапками й поза `{…}` виразів атрибутів.
fn tag_end(tag: &str) -> usize {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    for (index, ch) in tag.char_indices() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') if depth == 0 => quote = Some(ch),
            (None, '"') => quote = Some('"'),
            (None, '{') => depth += 1,
            (None, '}') => depth = depth.saturating_sub(1),
            (None, '>') if depth == 0 => return index + 1,
            _ => {}
        }
    }
    tag.len()
}

fn check_class(tag: &str, offset: usize, findings: &mut Vec<Finding>) {
    let Some(at) = tag.find("@class={") else {
        return;
    };
    let value = &tag[at + "@class=".len()..];
    let end = tag_end(&format!("<{value}"))
        .saturating_sub(1)
        .min(value.len());
    let value = &value[..end];
    for attribute in BOOLEAN_ATTRIBUTES {
        if value.contains(&format!("\"{attribute}\"")) {
            findings.push(Finding {
                offset: offset + at,
                message: format!(
                    "`{attribute}` у @class стає класом, а не атрибутом — елемент не буде {}",
                    match attribute {
                        "selected" => "вибраним",
                        "checked" => "позначеним",
                        "disabled" => "вимкненим",
                        _ => "таким, як задумано",
                    }
                ),
                hint: format!(
                    "для атрибутів-прапорців є @attr: @attr={{#{{ {attribute}: умова }}}}"
                ),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_braces_in_text_are_flagged_but_not_in_attributes() {
        let page = "---\nlet c = 1;\n---\n<td><a href={url(\"/c/\" + c.id)} hx-get={x}>{c.name}</a></td>\n<p>{{ c.name }}</p>";
        let found = lint(page);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].message.contains("`{c.name}`"),
            "{}",
            found[0].message
        );
        assert_eq!(&page[found[0].offset..found[0].offset + 8], "{c.name}");
        assert!(found[0].hint.contains("{{ c.name }}"), "{}", found[0].hint);
    }

    #[test]
    fn prose_css_and_raw_blocks_are_left_alone() {
        let page = "<p>Формат {рік-місяць} і {  } та {1,2}</p>\n\
            <style>.a { color: red }</style>\n<script>const o = {a};</script>\n\
            <rhaix:raw>{name}</rhaix:raw>\n<p>{{! коментар {x} }}</p>";
        assert_eq!(lint(page), vec![]);
    }

    #[test]
    fn boolean_attributes_in_class_are_flagged() {
        let page = r#"<select><option value="a" @class={#{"selected": s == "a"}}>A</option>
<option @attr={#{ selected: s == "b" }}>B</option>
<li @class={#{"active": on}}>x</li></select>"#;
        let found = lint(page);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].message.contains("selected"),
            "{}",
            found[0].message
        );
        assert!(found[0].hint.contains("@attr"), "{}", found[0].hint);
    }
}
