//! Одна діагностика на всі шари шаблонізатора.
//!
//! Правило M2 починає діяти вже тут: будь-яка помилка — парсингу, компіляції
//! виразу чи рантайму — несе спан у координатах `.rhx`, тому завжди може бути
//! показана як `файл:рядок:колонка` з підсвіченим фрагментом.

use rhaix_parser::{render_message, Source, Span};

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub message: String,
    pub span: Span,
    pub hint: Option<String>,
    /// Ланцюжок файлів: сторінка → компонент → вкладений компонент (M3).
    pub chain: Vec<String>,
}

impl Diagnostic {
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            span,
            hint: None,
            chain: Vec::new(),
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

    pub fn render(&self, source: &Source) -> String {
        let mut text = render_message(source, &self.message, self.span, self.hint.as_deref());
        if !self.chain.is_empty() {
            text.push_str(&format!("  = у ланцюжку: {}\n", self.chain.join(" → ")));
        }
        text
    }
}

pub type Result<T> = std::result::Result<T, Diagnostic>;
