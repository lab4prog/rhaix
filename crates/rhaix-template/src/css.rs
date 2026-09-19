//! Скоупинг CSS для `<style scoped>`.
//!
//! Правило одне: до **останнього** складеного селектора дописується
//! `[data-rhx-<хеш>]`, а рендерер ставить такий самий атрибут на кожен елемент,
//! що належить цьому компоненту. Той самий підхід, що у Vue, і з тієї ж
//! причини: скоупити лише останню частину означає, що `.card .title` усередині
//! компонента спрацює, а на чужу розмітку не пошириться.
//!
//! ```text
//! .card { }             → .card[data-rhx-ab12cd34] { }
//! .card .title { }      → .card .title[data-rhx-ab12cd34] { }
//! a:hover { }           → a[data-rhx-ab12cd34]:hover { }
//! @media (...) { .x{} } → @media (...) { .x[data-rhx-ab12cd34]{} }
//! ```
//!
//! Повноцінного парсера CSS тут немає й не треба: досить коректно розрізняти
//! рядки, коментарі та вкладеність дужок, а решту віддавати дослівно.

/// At-правила, усередину яких треба зайти й скоупити вміст.
const NESTED_AT_RULES: [&str; 5] = ["@media", "@supports", "@container", "@layer", "@scope"];

/// At-правила, вміст яких чіпати не можна: там не селектори.
const OPAQUE_AT_RULES: [&str; 5] = [
    "@keyframes",
    "@-webkit-keyframes",
    "@font-face",
    "@page",
    "@property",
];

/// Дописати атрибут скоупу до всіх селекторів у CSS.
pub fn scope_css(css: &str, attribute: &str) -> String {
    let mut out = String::with_capacity(css.len() + css.len() / 4);
    scope_block(css, attribute, &mut out);
    out
}

/// Обробити послідовність правил (тіло файлу або тіло `@media`).
fn scope_block(css: &str, attribute: &str, out: &mut String) {
    let bytes = css.as_bytes();
    let mut i = 0;
    // Початок поточної «прелюдії» — селектора або at-правила.
    let mut start = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = skip_comment(css, i);
            }
            b'"' | b'\'' => {
                i = skip_string(css, i);
            }
            b'{' => {
                let prelude = &css[start..i];
                let (body, after) = read_block(css, i);
                emit_rule(prelude, body, attribute, out);
                i = after;
                start = i;
            }
            b';' => {
                // `@import ...;` та інші правила без тіла — дослівно.
                out.push_str(&css[start..=i]);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    // Хвіст без тіла (зазвичай пробіли).
    out.push_str(&css[start..]);
}

/// Вивести одне правило: прелюдія + `{` + тіло + `}`.
fn emit_rule(prelude: &str, body: &str, attribute: &str, out: &mut String) {
    // Пробіли навколо зберігаємо: форматування автора не має «з'їхати» після
    // скоупингу — стиль ще читати людині.
    let trimmed_start = prelude.trim_start();
    let leading = &prelude[..prelude.len() - trimmed_start.len()];
    let trimmed = trimmed_start.trim_end();
    let trailing = &trimmed_start[trimmed.len()..];
    out.push_str(leading);

    if trimmed.starts_with('@') {
        let name = at_rule_name(trimmed);
        out.push_str(trimmed);
        out.push_str(trailing);
        out.push('{');
        if NESTED_AT_RULES.contains(&name.as_str()) {
            // Усередині — звичайні правила, їх треба скоупити.
            scope_block(body, attribute, out);
        } else if OPAQUE_AT_RULES.contains(&name.as_str()) {
            // `from`/`to`/`50%` у keyframes селекторами не є.
            out.push_str(body);
        } else {
            // Невідоме at-правило: безпечніше не чіпати, ніж зіпсувати.
            out.push_str(body);
        }
        out.push('}');
        return;
    }

    out.push_str(&scope_selector_list(trimmed, attribute));
    out.push_str(trailing);
    out.push('{');
    out.push_str(body);
    out.push('}');
}

fn at_rule_name(prelude: &str) -> String {
    prelude
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Скоупити список селекторів, розділених комами.
fn scope_selector_list(selectors: &str, attribute: &str) -> String {
    split_top_level_commas(selectors)
        .into_iter()
        .map(|selector| scope_selector(selector, attribute))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Дописати атрибут до останнього складеного селектора.
fn scope_selector(selector: &str, attribute: &str) -> String {
    let selector = selector.trim();
    if selector.is_empty() {
        return String::new();
    }

    let start = last_compound_start(selector);
    let (head, compound) = selector.split_at(start);
    // Атрибут іде перед псевдокласом/псевдоелементом: `a:hover` →
    // `a[attr]:hover`, інакше правило просто не спрацює.
    let insert_at = pseudo_start(compound).unwrap_or(compound.len());
    format!(
        "{head}{}{attribute}{}",
        &compound[..insert_at],
        &compound[insert_at..]
    )
}

/// Де починається останній складений селектор (після комбінатора).
fn last_compound_start(selector: &str) -> usize {
    let bytes = selector.as_bytes();
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut last = 0;
    let mut i = 0;

    while i < bytes.len() {
        let ch = bytes[i];
        match quote {
            Some(q) => {
                if ch == b'\\' {
                    i += 2;
                    continue;
                }
                if ch == q {
                    quote = None;
                }
            }
            None => match ch {
                b'"' | b'\'' => quote = Some(ch),
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                // Комбінатори розділяють складені селектори.
                b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'+' | b'~' if depth == 0 => {
                    last = i + 1;
                }
                _ => {}
            },
        }
        i += 1;
    }
    last
}

/// Де в складеному селекторі починається псевдо (`:hover`, `::before`).
fn pseudo_start(compound: &str) -> Option<usize> {
    let bytes = compound.as_bytes();
    let mut depth = 0i32;
    for (i, &ch) in bytes.iter().enumerate() {
        match ch {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b':' if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// Розбити список селекторів на комах верхнього рівня.
fn split_top_level_commas(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut start = 0;

    for (i, &ch) in bytes.iter().enumerate() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                }
            }
            None => match ch {
                b'"' | b'\'' => quote = Some(ch),
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                b',' if depth == 0 => {
                    parts.push(&text[start..i]);
                    start = i + 1;
                }
                _ => {}
            },
        }
    }
    parts.push(&text[start..]);
    parts
}

/// Прочитати тіло блока від `{` до парної `}`; повертає тіло й позицію після неї.
fn read_block(css: &str, open: usize) -> (&str, usize) {
    let bytes = css.as_bytes();
    let mut depth = 0i32;
    let mut i = open;

    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = skip_comment(css, i);
                continue;
            }
            b'"' | b'\'' => {
                i = skip_string(css, i);
                continue;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return (&css[open + 1..i], i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    // Незакритий блок — віддаємо решту як тіло.
    (&css[open + 1..], css.len())
}

fn skip_comment(css: &str, at: usize) -> usize {
    match css[at + 2..].find("*/") {
        Some(end) => at + 2 + end + 2,
        None => css.len(),
    }
}

fn skip_string(css: &str, at: usize) -> usize {
    let bytes = css.as_bytes();
    let quote = bytes[at];
    let mut i = at + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == quote {
            return i + 1;
        }
        i += 1;
    }
    css.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "[data-rhx-ab12cd34]";

    fn scoped(css: &str) -> String {
        scope_css(css, A)
    }

    #[test]
    fn simple_selectors_get_the_attribute() {
        assert_eq!(
            scoped(".card{color:red}"),
            ".card[data-rhx-ab12cd34]{color:red}"
        );
        assert_eq!(scoped("div{}"), "div[data-rhx-ab12cd34]{}");
    }

    #[test]
    fn only_the_last_compound_is_scoped() {
        // `.card .title` → стиль діє на .title усередині компонента.
        assert_eq!(
            scoped(".card .title{a:b}"),
            ".card .title[data-rhx-ab12cd34]{a:b}"
        );
        assert_eq!(scoped("ul > li{a:b}"), "ul > li[data-rhx-ab12cd34]{a:b}");
    }

    #[test]
    fn pseudo_classes_stay_last() {
        // Атрибут має стояти перед псевдо, інакше правило не спрацює.
        assert_eq!(scoped("a:hover{a:b}"), "a[data-rhx-ab12cd34]:hover{a:b}");
        assert_eq!(
            scoped("li::before{a:b}"),
            "li[data-rhx-ab12cd34]::before{a:b}"
        );
        assert_eq!(
            scoped("a:not(.x){a:b}"),
            "a[data-rhx-ab12cd34]:not(.x){a:b}"
        );
    }

    #[test]
    fn selector_lists_are_scoped_one_by_one() {
        assert_eq!(
            scoped("h1, h2 .sub{a:b}"),
            "h1[data-rhx-ab12cd34], h2 .sub[data-rhx-ab12cd34]{a:b}"
        );
        // Кома всередині :is(...) не ділить список.
        assert_eq!(
            scoped(":is(h1, h2){a:b}"),
            "[data-rhx-ab12cd34]:is(h1, h2){a:b}"
        );
    }

    #[test]
    fn media_queries_are_entered() {
        assert_eq!(
            scoped("@media (max-width: 40em){.card{a:b}}"),
            "@media (max-width: 40em){.card[data-rhx-ab12cd34]{a:b}}"
        );
    }

    #[test]
    fn keyframes_are_left_alone() {
        // `from`/`to` — не селектори; скоупити їх означає зламати анімацію.
        let css = "@keyframes spin{from{a:b}to{c:d}}";
        assert_eq!(scoped(css), css);

        let css = "@font-face{font-family:X}";
        assert_eq!(scoped(css), css);
    }

    #[test]
    fn imports_pass_through() {
        let css = "@import url(\"x.css\");.card{a:b}";
        assert_eq!(
            scoped(css),
            "@import url(\"x.css\");.card[data-rhx-ab12cd34]{a:b}"
        );
    }

    #[test]
    fn braces_inside_strings_do_not_break_parsing() {
        // `content: "}"` колись ламало б підрахунок дужок.
        let css = ".a{content:\"}\"}.b{a:b}";
        assert_eq!(
            scoped(css),
            ".a[data-rhx-ab12cd34]{content:\"}\"}.b[data-rhx-ab12cd34]{a:b}"
        );
    }

    #[test]
    fn comments_are_preserved_and_skipped() {
        let css = "/* } не дужка */.card{a:b}";
        assert_eq!(scoped(css), "/* } не дужка */.card[data-rhx-ab12cd34]{a:b}");
    }

    #[test]
    fn whitespace_and_newlines_survive() {
        let css = "\n  .card {\n    color: red;\n  }\n";
        assert_eq!(
            scoped(css),
            "\n  .card[data-rhx-ab12cd34] {\n    color: red;\n  }\n"
        );
    }
}
