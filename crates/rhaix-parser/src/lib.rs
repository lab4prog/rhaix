//! Джерела `.rhx`, спани та розділення frontmatter / розмітки.
//!
//! Цей крейт навмисно не знає нічого про HTML і про Rhai — він відповідає лише
//! за те, щоб будь-яка позиція в байтах могла бути показана користувачеві як
//! `файл:рядок:колонка` разом із самим рядком. Це фундамент для воріт M2:
//! жодна помилка не має показувати сиру позицію Rhai без прив'язки до `.rhx`.

use std::fmt;
use std::path::{Path, PathBuf};

/// Діапазон у байтах у нормалізованому тексті [`Source`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        debug_assert!(start <= end, "span start must not exceed end");
        Self { start, end }
    }

    pub fn empty(at: usize) -> Self {
        Self { start: at, end: at }
    }

    pub fn len(&self) -> usize {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Зсунути спан, отриманий у вкладеному фрагменті, у координати файлу.
    ///
    /// Саме цим будуть перекладатися позиції помилок Rhai з окремо
    /// скомпільованого `{{ ... }}` у позицію всередині `.rhx`.
    pub fn shift(&self, by: usize) -> Span {
        Span::new(self.start + by, self.end + by)
    }
}

/// Позиція, придатна для показу людині: рядок і колонка з одиниці.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineCol {
    pub line: usize,
    pub col: usize,
}

impl fmt::Display for LineCol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

/// Файл `.rhx`, завантажений у пам'ять, з попередньо порахованими початками рядків.
#[derive(Debug, Clone)]
pub struct Source {
    path: PathBuf,
    text: String,
    line_starts: Vec<usize>,
}

impl Source {
    /// Текст нормалізується: прибирається BOM, `\r\n` і `\r` стають `\n`.
    ///
    /// Нумерація рядків після цього збігається з тим, що людина бачить у
    /// редакторі, а всі спани рахуються вже в нормалізованому тексті.
    pub fn new(path: impl Into<PathBuf>, raw: impl AsRef<str>) -> Self {
        let text = normalize(raw.as_ref());
        let line_starts = line_starts(&text);
        Self {
            path: path.into(),
            text,
            line_starts,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Текст за спаном. Межі підрізаються: спан із чужого файлу має давати
    /// порожній рядок, а не паніку посеред запиту.
    pub fn slice(&self, span: Span) -> &str {
        let end = span.end.min(self.text.len());
        let start = span.start.min(end);
        if !self.text.is_char_boundary(start) || !self.text.is_char_boundary(end) {
            return "";
        }
        &self.text[start..end]
    }

    /// Кількість рядків (порожній файл — один рядок).
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Рядок і колонка для зсуву в байтах. Колонка рахується в символах,
    /// а не в байтах, щоб каретка не «їхала» на кирилиці.
    pub fn line_col(&self, offset: usize) -> LineCol {
        let offset = offset.min(self.text.len());
        let line_idx = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let line_start = self.line_starts[line_idx];
        let col = self.text[line_start..offset].chars().count() + 1;
        LineCol {
            line: line_idx + 1,
            col,
        }
    }

    /// Текст рядка (нумерація з одиниці), без символу переносу.
    pub fn line_text(&self, line: usize) -> &str {
        if line == 0 || line > self.line_starts.len() {
            return "";
        }
        let start = self.line_starts[line - 1];
        let end = self
            .line_starts
            .get(line)
            .map(|next| next - 1)
            .unwrap_or(self.text.len());
        &self.text[start..end]
    }
}

fn normalize(raw: &str) -> String {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    if raw.contains('\r') {
        raw.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        raw.to_owned()
    }
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    starts.extend(
        text.char_indices()
            .filter(|(_, ch)| *ch == '\n')
            .map(|(i, _)| i + 1),
    );
    starts
}

/// Помилка розбору з прив'язкою до місця у файлі.
#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
    pub hint: Option<String>,
}

impl ParseError {
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            span,
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// Результат розділення файлу на frontmatter і розмітку.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    /// Rhai-код між `---`, якщо блок є.
    pub frontmatter: Option<Span>,
    /// Уся розмітка після закривального `---` (або весь файл, якщо блоку немає).
    pub markup: Span,
}

/// Розділити `.rhx` на frontmatter і розмітку.
///
/// Відкривальний `---` має бути першим непорожнім рядком; закривальний — рядком,
/// що складається лише з `---`. Спани повертаються в координатах [`Source`],
/// тому позиції в Rhai-коді й у розмітці лишаються придатними для діагностики.
pub fn split(source: &Source) -> Result<Split, ParseError> {
    let text = source.text();
    let open_at = match first_significant_offset(text) {
        Some(at) => at,
        None => {
            return Ok(Split {
                frontmatter: None,
                markup: Span::new(0, text.len()),
            })
        }
    };

    if !is_fence_line(text, open_at) {
        return Ok(Split {
            frontmatter: None,
            markup: Span::new(0, text.len()),
        });
    }

    let body_start = match text[open_at..].find('\n') {
        Some(nl) => open_at + nl + 1,
        None => {
            return Err(ParseError::new(
                "frontmatter відкрито, але не закрито",
                Span::new(open_at, text.len()),
            )
            .with_hint("додайте рядок `---` після коду"))
        }
    };

    let mut cursor = body_start;
    while cursor <= text.len() {
        if is_fence_line(text, cursor) {
            let frontmatter = Span::new(body_start, cursor.saturating_sub(1).max(body_start));
            let after_fence = text[cursor..]
                .find('\n')
                .map(|nl| cursor + nl + 1)
                .unwrap_or(text.len());
            return Ok(Split {
                frontmatter: Some(frontmatter),
                markup: Span::new(after_fence, text.len()),
            });
        }
        match text[cursor..].find('\n') {
            Some(nl) => cursor += nl + 1,
            None => break,
        }
    }

    Err(ParseError::new(
        "frontmatter відкрито, але не закрито",
        Span::new(open_at, open_at + 3),
    )
    .with_hint("додайте рядок `---` після коду"))
}

/// Зсув першого непорожнього символу.
fn first_significant_offset(text: &str) -> Option<usize> {
    text.char_indices()
        .find(|(_, ch)| !ch.is_whitespace())
        .map(|(i, _)| i)
}

/// Чи є рядок, що починається зі зсуву `at`, огорожею `---`?
///
/// Хвостові пробіли дозволені, будь-що інше — ні: `--- ok` огорожею не є.
fn is_fence_line(text: &str, at: usize) -> bool {
    if at > text.len() || !text.is_char_boundary(at) {
        return false;
    }
    let line_end = text[at..]
        .find('\n')
        .map(|nl| at + nl)
        .unwrap_or(text.len());
    let line = &text[at..line_end];
    line.trim_end() == "---"
}

/// Відрендерити помилку у вигляді, придатному для консолі:
///
/// ```text
/// error: frontmatter відкрито, але не закрито
///   ┌─ pages/todo.rhx:1:1
///   │
/// 1 │ ---
///   │ ^^^
///   = додайте рядок `---` після коду
/// ```
pub fn render_diagnostic(source: &Source, error: &ParseError) -> String {
    render_message(source, &error.message, error.span, error.hint.as_deref())
}

/// Те саме, але для будь-якого повідомлення: цим користуються всі шари —
/// парсер шаблону, компілятор виразів і рантайм, щоб вигляд помилки був один.
pub fn render_message(source: &Source, message: &str, span: Span, hint: Option<&str>) -> String {
    let start = source.line_col(span.start);
    let line = source.line_text(start.line);
    let gutter = start.line.to_string().len().max(1);
    let pad = " ".repeat(gutter);

    let caret_indent: String = line
        .chars()
        .take(start.col.saturating_sub(1))
        .map(|ch| if ch == '\t' { '\t' } else { ' ' })
        .collect();
    let caret_len = source
        .slice(span)
        .chars()
        .take_while(|ch| *ch != '\n')
        .count()
        .max(1);

    let mut out = String::new();
    out.push_str(&format!("error: {message}\n"));
    out.push_str(&format!("{pad}┌─ {}:{}\n", source.path().display(), start));
    out.push_str(&format!("{pad}│\n"));
    out.push_str(&format!("{} │ {}\n", start.line, line));
    out.push_str(&format!(
        "{pad}│ {}{}\n",
        caret_indent,
        "^".repeat(caret_len)
    ));
    if let Some(hint) = hint {
        out.push_str(&format!("{pad}= {hint}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(text: &str) -> Source {
        Source::new("test.rhx", text)
    }

    #[test]
    fn splits_frontmatter_and_markup() {
        let s = src("---\nlet a = 1;\n---\n<h1>hi</h1>\n");
        let split = split(&s).unwrap();
        assert_eq!(s.slice(split.frontmatter.unwrap()), "let a = 1;");
        assert_eq!(s.slice(split.markup), "<h1>hi</h1>\n");
    }

    #[test]
    fn file_without_frontmatter_is_all_markup() {
        let s = src("<h1>hi</h1>");
        let split = split(&s).unwrap();
        assert!(split.frontmatter.is_none());
        assert_eq!(s.slice(split.markup), "<h1>hi</h1>");
    }

    #[test]
    fn empty_frontmatter_is_allowed() {
        let s = src("---\n---\n<p>x</p>");
        let split = split(&s).unwrap();
        assert_eq!(s.slice(split.frontmatter.unwrap()), "");
        assert_eq!(s.slice(split.markup), "<p>x</p>");
    }

    #[test]
    fn unterminated_frontmatter_is_an_error() {
        let s = src("---\nlet a = 1;\n<h1>hi</h1>");
        let err = split(&s).unwrap_err();
        assert!(err.message.contains("не закрито"));
        assert_eq!(s.line_col(err.span.start).line, 1);
    }

    #[test]
    fn horizontal_rule_in_markup_is_not_a_fence() {
        let s = src("<p>a</p>\n---\n<p>b</p>");
        let split = split(&s).unwrap();
        assert!(split.frontmatter.is_none());
    }

    #[test]
    fn crlf_and_bom_are_normalized() {
        let s = src("\u{feff}---\r\nlet a = 1;\r\n---\r\n<p>x</p>");
        let split = split(&s).unwrap();
        assert_eq!(s.slice(split.frontmatter.unwrap()), "let a = 1;");
        assert_eq!(s.slice(split.markup), "<p>x</p>");
    }

    #[test]
    fn line_col_counts_characters_not_bytes() {
        let s = src("<p>Привіт {{ ім'я }}</p>");
        let at = s.text().find("{{").unwrap();
        let pos = s.line_col(at);
        assert_eq!(pos.line, 1);
        assert_eq!(pos.col, 11);
    }

    #[test]
    fn line_text_returns_requested_line() {
        let s = src("a\nb\nc");
        assert_eq!(s.line_text(2), "b");
        assert_eq!(s.line_text(3), "c");
        assert_eq!(s.line_text(9), "");
    }

    #[test]
    fn span_shift_maps_nested_positions() {
        let s = src("---\nlet a = 1;\n---\n<p>{{ oops }}</p>");
        let split = split(&s).unwrap();
        // помилка на позиції 3 всередині виразу `{{ oops }}`
        let inner = Span::new(3, 7);
        let expr_at = s.text().find("oops").unwrap() - 3;
        let mapped = inner.shift(expr_at);
        assert_eq!(s.line_col(mapped.start).line, 4);
        assert!(s.slice(split.markup).contains("oops"));
    }

    #[test]
    fn diagnostic_points_at_the_source_line() {
        let s = src("---\nlet a = 1;\n<h1>hi</h1>");
        let err = split(&s).unwrap_err();
        let text = render_diagnostic(&s, &err);
        assert!(text.contains("test.rhx:1:1"), "{text}");
        assert!(text.contains("^^^"), "{text}");
    }
}
