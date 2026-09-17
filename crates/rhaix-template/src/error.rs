//! Одна діагностика на всі шари шаблонізатора.
//!
//! Правило M2 починає діяти вже тут: будь-яка помилка — парсингу, компіляції
//! виразу чи рантайму — несе спан у координатах `.rhx`, тому завжди може бути
//! показана як `файл:рядок:колонка` з підсвіченим фрагментом.

use std::sync::Arc;

use rhaix_parser::{render_message, Source, Span};

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub message: String,
    pub span: Span,
    pub hint: Option<String>,
    /// Ланцюжок файлів: сторінка → компонент → вкладений компонент.
    pub chain: Vec<String>,
    /// Файл, у якому стався збій. Без нього спан із компонента показувався б
    /// у координатах сторінки — тобто взагалі не там.
    pub source: Option<Arc<Source>>,
}

impl Diagnostic {
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            span,
            hint: None,
            chain: Vec::new(),
            source: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn in_frame(mut self, frame: impl Into<String>) -> Self {
        self.chain.push(frame.into());
        self
    }

    /// Прив'язати до файлу — але не перезаписувати те, що вже прив'язане:
    /// найглибший рівень знає краще.
    pub fn in_file(mut self, source: Arc<Source>) -> Self {
        if self.source.is_none() {
            self.source = Some(source);
        }
        self
    }

    /// Текст помилки з підсвіченим рядком того файлу, у якому вона сталася.
    ///
    /// Якщо файл невідомий (наприклад, його не вдалося прочитати), лишається
    /// саме повідомлення — без вигаданих координат.
    pub fn text(&self) -> String {
        match &self.source {
            Some(source) => self.render(source),
            None => {
                let mut out = format!("error: {}\n", self.message);
                if let Some(hint) = &self.hint {
                    out.push_str(&format!("  = {hint}\n"));
                }
                if !self.chain.is_empty() {
                    out.push_str(&format!("  = у ланцюжку: {}\n", self.chain.join(" → ")));
                }
                out
            }
        }
    }

    pub fn render(&self, fallback: &Source) -> String {
        let source = self.source.as_deref().unwrap_or(fallback);
        let mut text = render_message(source, &self.message, self.span, self.hint.as_deref());
        if !self.chain.is_empty() {
            text.push_str(&format!("  = у ланцюжку: {}\n", self.chain.join(" → ")));
        }
        text
    }
}

pub type Result<T> = std::result::Result<T, Diagnostic>;
