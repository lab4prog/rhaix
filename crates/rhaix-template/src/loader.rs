//! Пошук і компіляція компонентів.
//!
//! Компоненти резолвляться **під час компіляції сторінки**, а не при рендері:
//! невідомий `<TodoItem/>` і циклічна залежність мають бути помилкою ще до
//! першого запиту, а не лімітом рекурсії в рантаймі (як це було в
//! Node-RED-стартері з `maxRecursion: 10`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rhai::Engine;
use rhaix_parser::{Source, Span};

use crate::error::Diagnostic;
use crate::Template;

/// Звідки рендерер бере компоненти.
///
/// Трейт, а не структура, щоб шаблонізатор не залежав від файлової системи:
/// у тестах підставляється мапа в пам'яті.
pub trait Components {
    /// Знайти компонент за іменем тега (`Ui.Button`).
    fn resolve(&self, name: &str, span: Span) -> Result<Arc<Template>, Diagnostic>;
}

/// Проєкт без компонентів — усе, що з великої літери, дає зрозумілу помилку.
pub struct NoComponents;

impl Components for NoComponents {
    fn resolve(&self, name: &str, span: Span) -> Result<Arc<Template>, Diagnostic> {
        Err(
            Diagnostic::new(format!("компонент `<{name}>` не знайдено"), span)
                .with_hint("у цьому проєкті немає теки `components/`"),
        )
    }
}

/// Завантажувач компонентів із теки `components/`.
pub struct Loader {
    root: PathBuf,
    engine: Arc<Engine>,
    /// Компоненти, скомпільовані під час поточного завантаження сторінки.
    /// У M6 цей кеш стане постійним (шлях + mtime → `Arc<Template>`).
    cache: Mutex<HashMap<PathBuf, Arc<Template>>>,
    /// Стек файлів, які зараз компілюються — так ловляться цикли.
    stack: Mutex<Vec<PathBuf>>,
}

impl Loader {
    pub fn new(root: impl Into<PathBuf>, engine: Arc<Engine>) -> Self {
        Self {
            root: root.into(),
            engine,
            cache: Mutex::new(HashMap::new()),
            stack: Mutex::new(Vec::new()),
        }
    }

    pub fn components_dir(&self) -> PathBuf {
        self.root.join("components")
    }

    /// Завантажити сторінку або layout.
    pub fn load(&self, file: &Path) -> Result<Arc<Template>, Diagnostic> {
        self.compile_file(file, Span::new(0, 0))
    }

    /// `Ui.Button` → `components/ui/Button.rhx`.
    ///
    /// Резолв регістрочутливий **завжди**, незалежно від файлової системи:
    /// інакше проєкт, який зібрався на Windows, розвалиться на Linux
    /// (RISKS 2.9).
    fn component_path(&self, name: &str) -> PathBuf {
        let mut path = self.components_dir();
        let segments: Vec<&str> = name.split('.').collect();
        for (index, segment) in segments.iter().enumerate() {
            if index + 1 == segments.len() {
                path.push(format!("{segment}.rhx"));
            } else {
                path.push(segment.to_ascii_lowercase());
            }
        }
        path
    }

    fn compile_file(&self, file: &Path, span: Span) -> Result<Arc<Template>, Diagnostic> {
        let key = file.to_path_buf();

        if let Some(found) = self.cache.lock().expect("кеш не отруєний").get(&key) {
            return Ok(found.clone());
        }

        {
            let mut stack = self.stack.lock().expect("стек не отруєний");
            if stack.contains(&key) {
                let chain: Vec<String> = stack
                    .iter()
                    .chain(std::iter::once(&key))
                    .map(|path| self.display(path))
                    .collect();
                return Err(Diagnostic::new("циклічна залежність компонентів", span)
                    .with_hint(format!("ланцюжок: {}", chain.join(" → "))));
            }
            stack.push(key.clone());
        }

        let result = self.compile_uncached(file, span);

        self.stack.lock().expect("стек не отруєний").pop();

        let template = result?;
        self.cache
            .lock()
            .expect("кеш не отруєний")
            .insert(key, template.clone());
        Ok(template)
    }

    fn compile_uncached(&self, file: &Path, span: Span) -> Result<Arc<Template>, Diagnostic> {
        let raw = std::fs::read_to_string(file).map_err(|err| {
            Diagnostic::new(format!("не вдалося прочитати {}", self.display(file)), span)
                .with_hint(err.to_string())
        })?;
        let source = Arc::new(Source::new(self.display(file), raw));
        Template::compile_with(source, &self.engine, self).map(Arc::new)
    }

    /// Шлях у вигляді, зрозумілому людині: відносно кореня проєкту.
    fn display(&self, file: &Path) -> String {
        file.strip_prefix(&self.root)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/")
    }

    /// Найсхожіше ім'я серед наявних компонентів — для підказки в помилці.
    fn suggest(&self, name: &str) -> Option<String> {
        let target = name.rsplit('.').next().unwrap_or(name).to_ascii_lowercase();
        let mut best: Option<(usize, String)> = None;
        collect_components(
            &self.components_dir(),
            &self.components_dir(),
            &mut |found| {
                let candidate = found
                    .rsplit('.')
                    .next()
                    .unwrap_or(&found)
                    .to_ascii_lowercase();
                let distance = edit_distance(&target, &candidate);
                if distance <= target.len() / 2 + 1 {
                    match &best {
                        Some((best_distance, _)) if *best_distance <= distance => {}
                        _ => best = Some((distance, found.clone())),
                    }
                }
            },
        );
        best.map(|(_, name)| name)
    }
}

impl Components for Loader {
    fn resolve(&self, name: &str, span: Span) -> Result<Arc<Template>, Diagnostic> {
        let path = self.component_path(name);
        if !path.is_file() {
            let mut diagnostic = Diagnostic::new(format!("компонент `<{name}>` не знайдено"), span);
            diagnostic = match self.suggest(name) {
                Some(similar) => {
                    diagnostic.with_hint(format!("можливо, ви мали на увазі `<{similar}>`?"))
                }
                None => diagnostic.with_hint(format!("очікувався файл `{}`", self.display(&path))),
            };
            return Err(diagnostic);
        }
        self.compile_file(&path, span)
    }
}

/// Обійти `components/` і віддати імена у вигляді тегів (`Ui.Button`).
fn collect_components(dir: &Path, root: &Path, visit: &mut impl FnMut(String)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_components(&path, root, visit);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rhx") {
            continue;
        }
        if let Ok(relative) = path.strip_prefix(root) {
            let mut segments: Vec<String> = relative
                .iter()
                .map(|part| part.to_string_lossy().into_owned())
                .collect();
            if let Some(last) = segments.last_mut() {
                *last = last.trim_end_matches(".rhx").to_owned();
            }
            for segment in segments.iter_mut().rev().skip(1) {
                let mut chars = segment.chars();
                if let Some(first) = chars.next() {
                    *segment = first.to_uppercase().collect::<String>() + chars.as_str();
                }
            }
            visit(segments.join("."));
        }
    }
}

/// Відстань Левенштейна — щоб у помилці була підказка, а не лише констатація.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];

    for (i, ch_a) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, ch_b) in b.iter().enumerate() {
            let cost = usize::from(ch_a != ch_b);
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_names_map_to_files() {
        let loader = Loader::new("/app", Arc::new(Engine::new()));
        assert!(loader
            .component_path("TodoItem")
            .ends_with("components/TodoItem.rhx"));
        assert!(loader
            .component_path("Ui.Button")
            .ends_with("components/ui/Button.rhx"));
        assert!(loader
            .component_path("Forms.Field.Text")
            .ends_with("components/forms/field/Text.rhx"));
    }

    #[test]
    fn edit_distance_finds_typos() {
        assert_eq!(edit_distance("todoitem", "todoitm"), 1);
        assert_eq!(edit_distance("card", "card"), 0);
        assert!(edit_distance("card", "button") > 3);
    }
}
