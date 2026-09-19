//! `rhaix check` — компіляція всього проєкту без запуску сервера.
//!
//! Перевіряються ті самі речі, що й під час запиту: синтаксис розмітки й
//! frontmatter, невідомі компоненти, циклічні залежності. Різниця лише в тому,
//! що помилку видно одразу для всіх файлів, а не для того, який відкрили.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rhaix_script::{engine as build_engine, Limits};
use rhaix_template::{Loader, TemplateCache};

use crate::Config;

/// Одна знайдена проблема.
#[derive(Debug, Clone)]
pub struct Issue {
    pub file: String,
    pub line: usize,
    pub col: usize,
    pub message: String,
    pub hint: Option<String>,
    /// Готовий текст із підсвіченим рядком — те саме, що показує сервер.
    pub rendered: String,
}

impl Issue {
    /// Рядок JSON для `rhaix check --json`: машині потрібні поля, а не картинка.
    pub fn to_json(&self) -> String {
        let hint = match &self.hint {
            Some(hint) => format!("\"{}\"", escape(hint)),
            None => "null".to_owned(),
        };
        format!(
            "{{\"file\":\"{}\",\"line\":{},\"col\":{},\"message\":\"{}\",\"hint\":{}}}",
            escape(&self.file),
            self.line,
            self.col,
            escape(&self.message),
            hint
        )
    }
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// Перевірити всі `.rhx` проєкту.
pub fn check(config: &Config) -> Vec<Issue> {
    let engine = Arc::new(build_engine(Limits::default()));
    let cache = TemplateCache::watching();
    let mut issues = Vec::new();

    for file in collect_files(config) {
        let loader = Loader::new(config.root.clone(), engine.clone(), cache.clone());
        let Err(diagnostic) = loader.load(&file) else {
            continue;
        };

        let (line, col) = match &diagnostic.source {
            Some(source) => {
                let position = source.line_col(diagnostic.span.start);
                (position.line, position.col)
            }
            None => (0, 0),
        };
        issues.push(Issue {
            file: crate::display_path(&config.root, &file),
            line,
            col,
            message: diagnostic.message.clone(),
            hint: diagnostic.hint.clone(),
            rendered: diagnostic.text(),
        });
    }

    issues.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
    issues
}

/// Усі файли, які має сенс компілювати.
fn collect_files(config: &Config) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in [
        config.pages_dir(),
        config.partials_dir(),
        config.api_dir(),
        config.root.join("components"),
        config.root.join("layouts"),
    ] {
        walk(&dir, &mut files);
    }
    let middleware = config.middleware_path();
    if middleware.is_file() {
        files.push(middleware);
    }
    files.sort();
    files
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rhx") {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    fn fixture() -> Config {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixture");
        Config::new(root, SocketAddr::from(([127, 0, 0, 1], 0)))
    }

    #[test]
    fn check_finds_the_broken_files_of_the_fixture() {
        let issues = check(&fixture());

        // У фікстурі навмисно лежать зламані сторінки: невідомий компонент,
        // цикл і директива на компоненті.
        let files: Vec<&str> = issues.iter().map(|issue| issue.file.as_str()).collect();
        assert!(files.contains(&"pages/typo.rhx"), "{files:?}");
        assert!(files.contains(&"pages/cycle.rhx"), "{files:?}");
        assert!(files.contains(&"pages/badcomp.rhx"), "{files:?}");

        // А ось помилки рантайму (невідома змінна) чекають на запит: перевірка
        // компілює, але не виконує.
        assert!(!files.contains(&"pages/broken.rhx"), "{files:?}");
    }

    #[test]
    fn issues_carry_position_and_render_as_json() {
        let issues = check(&fixture());
        let typo = issues
            .iter()
            .find(|issue| issue.file == "pages/typo.rhx")
            .expect("сторінка з помилкою");

        assert_eq!(typo.line, 1);
        assert!(typo.col > 1, "колонка вказує на тег: {}", typo.col);
        assert!(typo.rendered.contains("^"), "{}", typo.rendered);

        let json = typo.to_json();
        assert!(json.contains("\"file\":\"pages/typo.rhx\""), "{json}");
        assert!(json.contains("\"line\":1"), "{json}");
        assert!(!json.contains('\n'), "JSON має бути одним рядком: {json}");
    }
}
