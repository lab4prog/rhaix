//! Пошук і компіляція компонентів.
//!
//! Компоненти резолвляться **під час компіляції сторінки**, а не при рендері:
//! невідомий `<TodoItem/>` і циклічна залежність мають бути помилкою ще до
//! першого запиту, а не лімітом рекурсії в рантаймі (як це було в
//! Node-RED-стартері з `maxRecursion: 10`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

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

/// Відбиток файлу, за яким видно, що його змінили.
///
/// Час зміни плюс розмір: редактори, що зберігають файл за ту саму секунду,
/// майже завжди міняють і довжину, тож разом ці двоє надійніші за кожного окремо.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    size: u64,
}

impl Stamp {
    fn of(path: &Path) -> Self {
        match std::fs::metadata(path) {
            Ok(meta) => Self {
                modified: meta.modified().ok(),
                size: meta.len(),
            },
            Err(_) => Self {
                modified: None,
                size: 0,
            },
        }
    }
}

/// Скомпільований шаблон разом із файлами, від яких він залежить.
struct CacheEntry {
    template: Arc<Template>,
    /// Сам файл і всі компоненти, що в нього потрапили. Зміна будь-кого з них
    /// робить запис несвіжим — інакше правка компонента не була б видна на
    /// сторінці, яка його вбудувала.
    stamps: Vec<(PathBuf, Stamp)>,
}

/// Спільний кеш шаблонів: живе стільки, скільки процес.
pub struct TemplateCache {
    entries: Mutex<HashMap<PathBuf, CacheEntry>>,
    /// У dev перевіряємо свіжість на кожен запит, у проді — ніколи.
    check_freshness: bool,
}

impl TemplateCache {
    /// Кеш для розробки: помічає зміни файлів.
    pub fn watching() -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(HashMap::new()),
            check_freshness: true,
        })
    }

    /// Кеш для продакшну: жодних звернень до файлової системи після компіляції.
    pub fn frozen() -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(HashMap::new()),
            check_freshness: false,
        })
    }

    /// Викинути все — так реагуємо на подію watcher-а.
    pub fn clear(&self) {
        self.entries.lock().expect("кеш не отруєний").clear();
    }

    pub fn len(&self) -> usize {
        self.entries.lock().expect("кеш не отруєний").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn get(&self, file: &Path) -> Option<Arc<Template>> {
        let entries = self.entries.lock().expect("кеш не отруєний");
        let entry = entries.get(file)?;
        if self.check_freshness
            && entry
                .stamps
                .iter()
                .any(|(path, stamp)| Stamp::of(path) != *stamp)
        {
            return None;
        }
        Some(entry.template.clone())
    }

    fn put(&self, file: PathBuf, template: Arc<Template>, deps: Vec<PathBuf>) {
        let stamps = deps
            .into_iter()
            .map(|path| {
                let stamp = Stamp::of(&path);
                (path, stamp)
            })
            .collect();
        self.entries
            .lock()
            .expect("кеш не отруєний")
            .insert(file, CacheEntry { template, stamps });
    }
}

/// Завантажувач компонентів із теки `components/`.
pub struct Loader {
    root: PathBuf,
    engine: Arc<Engine>,
    /// Спільний кеш між запитами.
    cache: Arc<TemplateCache>,
    /// Файли, скомпільовані під час поточного завантаження: і дедуплікація,
    /// і список залежностей для кешу.
    visited: Mutex<HashMap<PathBuf, Arc<Template>>>,
    /// Стек файлів, які зараз компілюються — так ловляться цикли.
    stack: Mutex<Vec<PathBuf>>,
}

impl Loader {
    pub fn new(root: impl Into<PathBuf>, engine: Arc<Engine>, cache: Arc<TemplateCache>) -> Self {
        Self {
            root: root.into(),
            engine,
            cache,
            visited: Mutex::new(HashMap::new()),
            stack: Mutex::new(Vec::new()),
        }
    }

    pub fn components_dir(&self) -> PathBuf {
        self.root.join("components")
    }

    /// Завантажити сторінку або layout.
    ///
    /// Результат кешується разом зі списком залежностей, тому правка
    /// компонента робить несвіжими всі сторінки, що його вбудували.
    pub fn load(&self, file: &Path) -> Result<Arc<Template>, Diagnostic> {
        if let Some(cached) = self.cache.get(file) {
            return Ok(cached);
        }

        let template = self.compile_file(file, Span::new(0, 0))?;

        let deps: Vec<PathBuf> = self
            .visited
            .lock()
            .expect("список файлів не отруєний")
            .keys()
            .cloned()
            .collect();
        self.cache.put(file.to_path_buf(), template.clone(), deps);
        Ok(template)
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

        if let Some(found) = self
            .visited
            .lock()
            .expect("список файлів не отруєний")
            .get(&key)
        {
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
        self.visited
            .lock()
            .expect("список файлів не отруєний")
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
        let loader = Loader::new("/app", Arc::new(Engine::new()), TemplateCache::watching());
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
    fn cache_returns_the_same_tree_and_notices_changes() {
        let dir = std::env::temp_dir().join(format!("rhaix-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("components")).unwrap();
        let page = dir.join("page.rhx");
        std::fs::write(&page, "<p><Box /></p>").unwrap();
        std::fs::write(dir.join("components/Box.rhx"), "<b>перше</b>").unwrap();

        let engine = Arc::new(rhaix_script::engine(rhaix_script::Limits::default()));
        let cache = TemplateCache::watching();

        let first = Loader::new(&dir, engine.clone(), cache.clone())
            .load(&page)
            .unwrap();
        let second = Loader::new(&dir, engine.clone(), cache.clone())
            .load(&page)
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second), "друге завантаження — з кешу");

        // Правка компонента має робити несвіжою сторінку, яка його вбудувала.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("components/Box.rhx"), "<b>друге і довше</b>").unwrap();
        let third = Loader::new(&dir, engine, cache).load(&page).unwrap();
        assert!(
            !Arc::ptr_eq(&first, &third),
            "після правки — перекомпіляція"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn edit_distance_finds_typos() {
        assert_eq!(edit_distance("todoitem", "todoitm"), 1);
        assert_eq!(edit_distance("card", "card"), 0);
        assert!(edit_distance("card", "button") > 3);
    }
}
