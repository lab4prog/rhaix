//! Проєкт під файлом: де його корінь, які є компоненти, чи компілюється файл.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rhaix_parser::Source;
use rhaix_script::{engine as build_engine, Limits};
use rhaix_template::{Diagnostic, Loader, Template, TemplateCache};

/// Корінь проєкту для цього файлу.
///
/// Шукаємо `rhaix.toml` вгору по деревах; якщо його немає — беремо теку, що
/// містить `pages/`. Так сервер працює і в проєкті без конфігу.
pub fn find_root(file: &Path) -> Option<PathBuf> {
    let mut dir = file.parent()?;
    loop {
        if dir.join("rhaix.toml").is_file() || dir.join("pages").is_dir() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// Скомпілювати **текст із редактора** (а не з диска) у контексті проєкту.
///
/// Компоненти резолвляться з диска через звичайний `Loader`, тому
/// `<TodoItem/>` у незбереженому буфері перевіряється так само, як під час
/// запиту. Повертає помилку компіляції, якщо вона є.
pub fn compile(root: &Path, file: &Path, text: &str) -> Result<Arc<Template>, Diagnostic> {
    let engine = Arc::new(build_engine(Limits::default()));
    // Свіжий кеш на кожну перевірку: у редакторі файли змінюються постійно, і
    // застарілий компонент показував би помилку, якої вже немає.
    let loader = Loader::new(
        root.to_path_buf(),
        engine.clone(),
        TemplateCache::watching(),
    );
    let source = Arc::new(Source::new(file, text));
    Template::compile_with(source, &engine, &loader).map(Arc::new)
}

/// Файл компонента за іменем тега: `Ui.Card` → `components/ui/Card.rhx`.
///
/// Те саме правило, що в резолвері шаблонізатора: крапка — роздільник тек,
/// сегменти тек у нижньому регістрі, останній сегмент — ім'я файлу як є.
pub fn component_path(root: &Path, name: &str) -> PathBuf {
    let mut path = root.join("components");
    let parts: Vec<&str> = name.split('.').collect();
    for segment in &parts[..parts.len().saturating_sub(1)] {
        path.push(segment.to_lowercase());
    }
    if let Some(last) = parts.last() {
        path.push(format!("{last}.rhx"));
    }
    path
}

/// Усі компоненти проєкту як імена тегів: `TodoItem`, `Ui.Card`.
pub fn component_names(root: &Path) -> Vec<String> {
    let dir = root.join("components");
    let mut names = Vec::new();
    walk(&dir, &dir, &mut names);
    names.sort();
    names.dedup();
    names
}

fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, base, out);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rhx") {
            continue;
        }
        let Ok(relative) = path.strip_prefix(base) else {
            continue;
        };
        // `ui/Card.rhx` → `Ui.Card`: теки з великої літери, файл — як є.
        let mut parts: Vec<String> = Vec::new();
        let count = relative.components().count();
        for (index, part) in relative.components().enumerate() {
            let text = part.as_os_str().to_string_lossy();
            if index + 1 == count {
                parts.push(text.trim_end_matches(".rhx").to_owned());
            } else {
                let mut chars = text.chars();
                if let Some(first) = chars.next() {
                    parts.push(first.to_uppercase().collect::<String>() + chars.as_str());
                }
            }
        }
        out.push(parts.join("."));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_paths_follow_the_resolver_rule() {
        let root = Path::new("/app");
        assert_eq!(
            component_path(root, "TodoItem"),
            Path::new("/app/components/TodoItem.rhx")
        );
        assert_eq!(
            component_path(root, "Ui.Card"),
            Path::new("/app/components/ui/Card.rhx")
        );
        assert_eq!(
            component_path(root, "Forms.Field.Text"),
            Path::new("/app/components/forms/field/Text.rhx")
        );
    }

    #[test]
    fn root_is_found_by_walking_up() {
        // Фікстура сервера має і rhaix.toml, і pages/.
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rhaix-server/tests/fixture");
        let page = fixture.join("pages/index.rhx");
        let root = find_root(&page).expect("корінь має знайтись");
        assert!(root.join("rhaix.toml").is_file(), "{root:?}");
    }

    #[test]
    fn component_names_include_nested_directories() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rhaix-server/tests/fixture");
        let names = component_names(&fixture);
        assert!(names.contains(&"Greeting".to_owned()), "{names:?}");
        assert!(names.contains(&"Ui.Card".to_owned()), "{names:?}");
    }

    #[test]
    fn a_broken_buffer_reports_a_diagnostic() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rhaix-server/tests/fixture");
        let file = fixture.join("pages/scratch.rhx");

        // Невідомий компонент — помилка компіляції, ще до збереження файлу.
        let err = compile(&fixture, &file, "<NoSuchComponent />").unwrap_err();
        assert!(err.message.contains("NoSuchComponent"), "{}", err.message);

        // А коректний текст компілюється.
        assert!(compile(&fixture, &file, "<p>{{ 1 + 1 }}</p>").is_ok());
    }
}
