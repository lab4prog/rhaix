//! Звідки беруться файли проєкту.
//!
//! У розробці це диск, у зібраному бінарнику — таблиця, вшита в сам виконуваний
//! файл. Решта коду про різницю не знає: і завантажувач шаблонів, і сканер
//! маршрутів, і статика ходять через один трейт.

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Доступ до файлів проєкту тільки на читання.
pub trait Files: Send + Sync {
    /// Вміст файлу як байти.
    fn read(&self, path: &Path) -> Option<Vec<u8>>;

    /// Чи є такий файл.
    fn exists(&self, path: &Path) -> bool;

    /// Усі файли в теці (рекурсивно) із заданим розширенням.
    fn list(&self, dir: &Path, extension: &str) -> Vec<PathBuf>;

    /// Текстовий вміст — те, що потрібно шаблонам.
    fn read_text(&self, path: &Path) -> Option<String> {
        self.read(path)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// Звичайна файлова система.
#[derive(Debug, Default, Clone, Copy)]
pub struct DiskFiles;

impl DiskFiles {
    pub fn shared() -> Arc<dyn Files> {
        Arc::new(Self)
    }
}

impl Files for DiskFiles {
    fn read(&self, path: &Path) -> Option<Vec<u8>> {
        std::fs::read(path).ok()
    }

    fn exists(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn list(&self, dir: &Path, extension: &str) -> Vec<PathBuf> {
        fn walk(dir: &Path, extension: &str, out: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, extension, out);
                } else if path.extension().and_then(|e| e.to_str()) == Some(extension) {
                    out.push(path);
                }
            }
        }

        let mut found = Vec::new();
        walk(dir, extension, &mut found);
        found.sort();
        found
    }
}

/// Файли, вшиті в бінарник.
///
/// Шляхи тут відносні до кореня проєкту й завжди з прямими слешами — саме в
/// такому вигляді їх записує `rhaix build`.
pub struct EmbeddedFiles {
    entries: Vec<(String, &'static [u8])>,
}

impl EmbeddedFiles {
    /// Зібрати таблицю файлів. Повертається одразу `Arc<dyn Files>`, бо іншого
    /// застосування в неї немає: її кладуть у конфіг і більше не чіпають.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(entries: &[(&'static str, &'static [u8])]) -> Arc<dyn Files> {
        Arc::new(Self {
            entries: entries
                .iter()
                .map(|(path, bytes)| (normalise(Path::new(path)), *bytes))
                .collect(),
        })
    }

    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(path, _)| path.as_str())
    }
}

impl Files for EmbeddedFiles {
    fn read(&self, path: &Path) -> Option<Vec<u8>> {
        let wanted = normalise(path);
        self.entries
            .iter()
            .find(|(name, _)| *name == wanted)
            .map(|(_, bytes)| bytes.to_vec())
    }

    fn exists(&self, path: &Path) -> bool {
        let wanted = normalise(path);
        self.entries.iter().any(|(name, _)| *name == wanted)
    }

    fn list(&self, dir: &Path, extension: &str) -> Vec<PathBuf> {
        let prefix = normalise(dir);
        let suffix = format!(".{extension}");
        let mut found: Vec<PathBuf> = self
            .entries
            .iter()
            .filter(|(name, _)| {
                (prefix.is_empty() || name.starts_with(&format!("{prefix}/")))
                    && name.ends_with(&suffix)
            })
            .map(|(name, _)| PathBuf::from(name))
            .collect();
        found.sort();
        found
    }
}

/// Шлях у канонічному вигляді: прямі слеші, без `./` на початку.
fn normalise(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    text.trim_start_matches("./").to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn embedded() -> Arc<dyn Files> {
        EmbeddedFiles::new(&[
            ("pages/index.rhx", "<h1>привіт</h1>".as_bytes()),
            (
                "pages/todo/[id].rhx",
                "<p>{{ req.param(\"id\") }}</p>".as_bytes(),
            ),
            ("public/style.css", b"body{margin:0}"),
            ("rhaix.toml", b"[db]\n"),
        ])
    }

    #[test]
    fn embedded_files_answer_like_a_disk() {
        let files = embedded();

        assert!(files.exists(Path::new("pages/index.rhx")));
        assert!(!files.exists(Path::new("pages/missing.rhx")));
        assert_eq!(
            files.read_text(Path::new("pages/index.rhx")).unwrap(),
            "<h1>привіт</h1>"
        );

        let pages = files.list(Path::new("pages"), "rhx");
        assert_eq!(pages.len(), 2, "{pages:?}");
        assert!(files.list(Path::new("public"), "css").len() == 1);
    }

    #[test]
    fn windows_style_paths_find_embedded_files() {
        // Сервер будує шляхи через `PathBuf::join`, тож на Windows вони
        // приходять зі зворотними слешами — а вшиті записані з прямими.
        let files = embedded();
        assert!(files.exists(Path::new("pages\\index.rhx")));
    }
}
