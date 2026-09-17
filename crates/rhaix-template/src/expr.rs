//! Вирази шаблону: компіляція, швидкий шлях і перенесення позицій Rhai у `.rhx`.
//!
//! Кожен `{{ ... }}` і кожне `={ ... }` компілюється **один раз** при
//! завантаженні файлу. Найважливіше тут — не швидкість, а те, що позиція
//! помилки з Rhai (рядок/колонка всередині фрагмента) перекладається у
//! позицію всередині `.rhx` — ворота M2.

use rhai::{Dynamic, Engine, EvalAltResult, Position, Scope, AST};
use rhaix_parser::{Source, Span};

use crate::error::{Diagnostic, Result};

/// Швидкий шлях для тривіальних виразів.
///
/// Вимір M0: звернення до поля коштує ~73 нс, виклик `eval_ast_with_scope` —
/// ~1 мкс. Тому голий `{{ row.title }}` не має проходити через рушій.
#[derive(Debug, Clone)]
pub enum Fast {
    /// `{{ title }}`
    Var(String),
    /// `{{ row.title }}`
    Field(String, String),
    /// Усе інше — через рушій.
    None,
}

#[derive(Debug, Clone)]
pub struct Expr {
    ast: AST,
    span: Span,
    source: String,
    fast: Fast,
}

impl Expr {
    /// Скомпілювати вміст `{{ ... }}` зі спаном у координатах файлу.
    pub fn compile(engine: &Engine, source: &Source, span: Span) -> Result<Self> {
        let text = source.slice(span);
        if text.trim().is_empty() {
            return Err(Diagnostic::new("порожній вираз", span)
                .with_hint("напишіть значення, яке треба вивести, наприклад `{{ user.name }}`"));
        }

        let ast = engine.compile_expression(text).map_err(|err| {
            let position = err.1;
            let at = map_position(text, span, position);
            let message = err.0.to_string();
            let hint = if message.contains("Unexpected") {
                Some("в інтерполяції дозволений лише вираз: без `let`, `;` і циклів".to_owned())
            } else {
                None
            };
            let diagnostic = Diagnostic::new(message, at);
            match hint {
                Some(hint) => diagnostic.with_hint(hint),
                None => diagnostic,
            }
        })?;

        Ok(Self {
            fast: detect_fast(text),
            ast,
            span,
            source: text.to_owned(),
        })
    }

    /// Скомпілювати frontmatter: тут, на відміну від інтерполяції,
    /// інструкції дозволені — це звичайний скрипт.
    pub fn compile_script(engine: &Engine, source: &Source, span: Span) -> Result<Self> {
        let text = source.slice(span);
        let ast = engine.compile(text).map_err(|err| {
            let at = map_position(text, span, err.1);
            Diagnostic::new(clean_message(&err.0.to_string()), at)
        })?;

        Ok(Self {
            ast,
            span,
            source: text.to_owned(),
            fast: Fast::None,
        })
    }

    pub fn span(&self) -> Span {
        self.span
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn fast(&self) -> &Fast {
        &self.fast
    }

    /// Обчислити вираз. Швидкий шлях тут не використовується: він потрібен
    /// рендереру, який уміє писати значення в буфер без клонування.
    pub fn eval(&self, engine: &Engine, scope: &mut Scope) -> Result<Dynamic> {
        engine
            .eval_ast_with_scope::<Dynamic>(scope, &self.ast)
            .map_err(|err| self.diagnose(&err))
    }

    /// Перекласти помилку рантайму в діагностику з позицією у файлі.
    pub fn diagnose(&self, err: &EvalAltResult) -> Diagnostic {
        let at = map_position(&self.source, self.span, err.position());
        let message = clean_message(&err.to_string());
        let mut diagnostic = Diagnostic::new(message, at);
        match err {
            EvalAltResult::ErrorVariableNotFound(name, _) => {
                diagnostic = diagnostic.with_hint(format!(
                    "змінної `{name}` немає в цьому файлі; перевірте frontmatter або props"
                ));
            }
            EvalAltResult::ErrorTooManyOperations(_) => {
                diagnostic = diagnostic
                    .with_hint("перевірте умову циклу: обмеження спрацювало до завершення");
            }
            EvalAltResult::ErrorTerminated(..) => {
                diagnostic =
                    diagnostic.with_hint("сторінка не вклалась у відведений час (5 секунд)");
            }
            _ => {}
        }
        diagnostic
    }
}

/// Rhai повідомляє позицію рядком і колонкою всередині того тексту, який ми йому
/// дали. Оскільки ми компілюємо точний зріз файлу, достатньо перерахувати
/// (рядок, колонка) у зсув і додати початок фрагмента.
fn map_position(fragment: &str, span: Span, position: Position) -> Span {
    let (Some(line), Some(col)) = (position.line(), position.position()) else {
        return span;
    };

    let mut offset = 0usize;
    for (index, text) in fragment.split('\n').enumerate() {
        if index + 1 == line {
            let col_offset: usize = text
                .chars()
                .take(col.saturating_sub(1))
                .map(|ch| ch.len_utf8())
                .sum();
            offset += col_offset;
            let start = span.start + offset.min(span.len());
            let word_len = fragment[offset.min(fragment.len())..]
                .chars()
                .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
                .map(|ch| ch.len_utf8())
                .sum::<usize>()
                .max(1);
            return Span::new(start, (start + word_len).min(span.end).max(start));
        }
        offset += text.len() + 1;
    }
    span
}

/// Прибрати з тексту помилки Rhai службовий хвіст `(line 1, position 7)`:
/// позицію ми вже показуємо кареткою у файлі.
fn clean_message(message: &str) -> String {
    let text = match message.find(" (line ") {
        Some(index) => message[..index].trim_end(),
        None => message,
    };
    translate(text)
}

/// Найчастіші повідомлення Rhai — українською.
///
/// Людина, яка пише `.rhx`, не знає ні Rust, ні Rhai, тому «Variable not found»
/// для неї не пояснення. Решта повідомлень поки лишається як є; повний переклад
/// разом із перемикачем `lang` — у M6.
fn translate(message: &str) -> String {
    let pairs: [(&str, &str); 8] = [
        ("Variable not found: ", "невідома змінна `"),
        ("Function not found: ", "невідома функція `"),
        ("Property not found: ", "невідома властивість `"),
        ("Unknown operator: ", "невідомий оператор `"),
        ("Array index ", "індекс масиву "),
        (
            "Number of operations exceeds",
            "скрипт виконав забагато операцій —",
        ),
        ("Script terminated: ", "виконання зупинено: "),
        ("Data race detected", "одночасний доступ до значення"),
    ];
    for (prefix, replacement) in pairs {
        if let Some(rest) = message.strip_prefix(prefix) {
            return if replacement.ends_with('`') {
                format!("{replacement}{rest}`")
            } else {
                format!("{replacement}{rest}")
            };
        }
    }
    if let Some(rest) = message.strip_prefix("Unexpected ") {
        return format!("несподіване {rest}");
    }
    if message.starts_with("Too many operations") {
        return "скрипт виконав забагато операцій — схоже на нескінченний цикл".to_owned();
    }
    message.to_owned()
}

fn detect_fast(text: &str) -> Fast {
    let trimmed = text.trim();
    if is_identifier(trimmed) {
        return Fast::Var(trimmed.to_owned());
    }
    if let Some((head, tail)) = trimmed.split_once('.') {
        if is_identifier(head) && is_identifier(tail) {
            return Fast::Field(head.to_owned(), tail.to_owned());
        }
    }
    Fast::None
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(ch) if ch.is_alphabetic() || ch == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_alphanumeric() || ch == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhaix_script::{engine, Limits};

    fn compile(text: &str) -> Result<Expr> {
        let source = Source::new("test.rhx", format!("<p>{text}</p>"));
        let span = Span::new(3, 3 + text.len());
        Expr::compile(&engine(Limits::default()), &source, span)
    }

    #[test]
    fn detects_fast_paths() {
        assert!(matches!(compile("title").unwrap().fast(), Fast::Var(name) if name == "title"));
        assert!(
            matches!(compile("row.title").unwrap().fast(), Fast::Field(var, field)
                if var == "row" && field == "title")
        );
        assert!(matches!(compile("a + b").unwrap().fast(), Fast::None));
        assert!(matches!(
            compile("row.items[0]").unwrap().fast(),
            Fast::None
        ));
    }

    #[test]
    fn statements_are_rejected_with_a_hint() {
        let err = compile("let x = 1").unwrap_err();
        assert!(err.hint.is_some(), "{err:?}");
    }

    #[test]
    fn empty_expression_is_an_error() {
        assert!(compile("   ").is_err());
    }

    #[test]
    fn runtime_error_points_into_the_file() {
        let source = Source::new("test.rhx", "<p>{{ oops }}</p>");
        let engine = engine(Limits::default());
        let expr = Expr::compile(&engine, &source, Span::new(6, 10)).unwrap();
        let mut scope = Scope::new();
        let diagnostic = expr.eval(&engine, &mut scope).unwrap_err();

        let position = source.line_col(diagnostic.span.start);
        assert_eq!(position.line, 1);
        assert_eq!(position.col, 7, "каретка має стояти на `oops`");
        assert!(diagnostic.hint.is_some());
        assert!(
            diagnostic.message.starts_with("невідома змінна"),
            "{}",
            diagnostic.message
        );
        assert!(
            !diagnostic.message.contains("line 1"),
            "{}",
            diagnostic.message
        );
    }
}
