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

/// Наскільки серйозно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Файл не скомпілюється — сторінка впаде на першому ж запиті.
    Error,
    /// Працюватиме, але не так, як, найімовірніше, задумано.
    Warning,
}

/// Одна знайдена проблема.
#[derive(Debug, Clone)]
pub struct Issue {
    pub severity: Severity,
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
        let severity = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        format!(
            "{{\"severity\":\"{severity}\",\"file\":\"{}\",\"line\":{},\"col\":{},\"message\":\"{}\",\"hint\":{}}}",
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
            severity: Severity::Error,
            file: crate::display_path(&config.root, &file),
            line,
            col,
            message: diagnostic.message.clone(),
            hint: diagnostic.hint.clone(),
            rendered: diagnostic.text(),
        });
    }

    issues.extend(layout_warnings(config));
    issues.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
    issues
}

/// Layout, який компілюється, але тихо ламає клієнт.
///
/// Без `<rhaix:head/>` фреймворку нікуди поставити htmx і свої скрипти в
/// `<head>`: вони їдуть у `<rhaix:scripts/>` у кінці `<body>`, а там кожен
/// boosted-перехід виконує їх знову (саме так у 1.2.5 множились тости). А
/// без обох тегів htmx не підключається взагалі — і жоден `hx-*` не працює,
/// хоч помилки ніде й не видно.
fn layout_warnings(config: &Config) -> Vec<Issue> {
    let mut files = Vec::new();
    walk(&config.root.join("layouts"), &mut files);
    files.sort();

    let mut warnings = Vec::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let has_head = text.contains("<rhaix:head");
        let has_scripts = text.contains("<rhaix:scripts");
        if has_head {
            continue;
        }
        // Показуємо на `<head>`, якщо він є: саме туди тег і треба дописати.
        let line = text
            .find("<head")
            .map(|at| text[..at].matches('\n').count() + 1)
            .unwrap_or(1);
        let name = crate::display_path(&config.root, &file);
        let (message, hint) = if has_scripts {
            (
                "у layout немає <rhaix:head/>: htmx і скрипти фреймворку стоять у кінці \
                 <body> і перевиконуються на кожному boosted-переході"
                    .to_owned(),
                "допишіть <rhaix:head/> усередину <head> — туди підуть стилі й скрипти".to_owned(),
            )
        } else {
            (
                "у layout немає ні <rhaix:head/>, ні <rhaix:scripts/>: htmx не \
                 підключиться, і жоден hx-* атрибут не працюватиме"
                    .to_owned(),
                "допишіть <rhaix:head/> у <head> і <rhaix:scripts/> перед </body>".to_owned(),
            )
        };
        let rendered = format!("попередження: {name}:{line}\n  {message}\n  підказка: {hint}\n");
        warnings.push(Issue {
            severity: Severity::Warning,
            file: name,
            line,
            col: 1,
            message,
            hint: Some(hint),
            rendered,
        });
    }
    warnings
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
    fn a_layout_without_rhaix_head_is_a_warning_not_an_error() {
        let root = std::env::temp_dir().join(format!("rhaix-check-{}", std::process::id()));
        let layouts = root.join("layouts");
        std::fs::create_dir_all(&layouts).unwrap();
        std::fs::write(
            layouts.join("main.rhx"),
            "<!DOCTYPE html>\n<html>\n<head><title>x</title></head>\n<body><slot/><rhaix:scripts/></body>\n</html>\n",
        )
        .unwrap();
        std::fs::write(
            layouts.join("bare.rhx"),
            "<html><body><slot/></body></html>\n",
        )
        .unwrap();
        std::fs::write(
            layouts.join("good.rhx"),
            "<html><head><rhaix:head/></head><body><slot/><rhaix:scripts/></body></html>\n",
        )
        .unwrap();

        let config = Config::new(root.clone(), SocketAddr::from(([127, 0, 0, 1], 0)));
        let issues = check(&config);
        std::fs::remove_dir_all(&root).ok();

        assert!(
            issues.iter().all(|i| i.severity == Severity::Warning),
            "{issues:?}"
        );
        let main = issues
            .iter()
            .find(|i| i.file == "layouts/main.rhx")
            .expect("main");
        assert_eq!(main.line, 3, "показуємо на <head>");
        assert!(main.message.contains("boosted"), "{}", main.message);
        let bare = issues
            .iter()
            .find(|i| i.file == "layouts/bare.rhx")
            .expect("bare");
        assert!(bare.message.contains("htmx не"), "{}", bare.message);
        assert!(
            !issues.iter().any(|i| i.file == "layouts/good.rhx"),
            "{issues:?}"
        );
        assert!(main.to_json().contains("\"severity\":\"warning\""));
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
