//! Невідомі ключі в `rhaix.toml`.
//!
//! `serde` мовчки пропускає поля, яких немає в структурі. Для конфігу це
//! пастка: `minify_html = true` чи `sesion_days = 7` виглядають як робоче
//! налаштування, а насправді їх ніхто не читає — і людина думає, що HTML
//! мінімізується чи сесія коротка. Тому все, чого фреймворк не знає,
//! називаємо вголос: у лозі при старті й попередженням у `rhaix check`.

/// Що фреймворк читає: секція → ключі.
const KNOWN: &[(&str, &[&str])] = &[
    ("server", &["port", "trust_proxy"]),
    ("db", &["driver", "url"]),
    (
        "app",
        &[
            "secret",
            "csrf",
            "session_cookie",
            "session_days",
            "session_secure",
            "tz_offset",
            "http_timeout",
            "locale",
        ],
    ),
    (
        "mail",
        &["from", "smtp_host", "smtp_port", "smtp_user", "smtp_pass"],
    ),
    ("api", &["cors", "title", "version", "openapi"]),
];

/// Один невідомий ключ або секція.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownKey {
    /// `[app] minify_html` або `[cache]`.
    pub name: String,
    pub line: usize,
    /// Найближчий відомий варіант — для опечаток.
    pub suggestion: Option<String>,
}

impl UnknownKey {
    pub fn message(&self) -> String {
        let mut text = format!(
            "rhaix.toml: `{}` фреймворк не читає — налаштування не діє",
            self.name
        );
        if let Some(suggestion) = &self.suggestion {
            text.push_str(&format!(" (можливо, `{suggestion}`?)"));
        }
        text
    }
}

/// Відстань редагування — для підказки «можливо, ви мали на увазі».
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let current = row[j + 1];
            row[j + 1] = (previous + usize::from(ca != *cb))
                .min(row[j] + 1)
                .min(current + 1);
            previous = current;
        }
    }
    row[b.len()]
}

fn closest<'a>(name: &str, options: impl Iterator<Item = &'a str>) -> Option<String> {
    options
        .map(|option| (distance(name, option), option))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, _)| *d)
        .map(|(_, option)| option.to_owned())
}

/// Знайти невідоме. Розбір рядковий: конфіг rhaix плаский, а номер рядка
/// потрібен для повідомлення — `toml::Value` його не дає.
pub fn unknown_keys(text: &str) -> Vec<UnknownKey> {
    let mut found = Vec::new();
    let mut section: Option<&str> = None;
    let mut known_keys: &[&str] = &[];
    let mut skip_section = false;

    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.split(']').next()) {
            let name = name.trim();
            match KNOWN.iter().find(|(known, _)| *known == name) {
                Some((known, keys)) => {
                    section = Some(known);
                    known_keys = keys;
                    skip_section = false;
                }
                None => {
                    found.push(UnknownKey {
                        name: format!("[{name}]"),
                        line: index + 1,
                        suggestion: closest(name, KNOWN.iter().map(|(s, _)| *s))
                            .map(|s| format!("[{s}]")),
                    });
                    // Ключі невідомої секції окремо не називаємо: досить секції.
                    skip_section = true;
                }
            }
            continue;
        }
        if skip_section {
            continue;
        }
        let Some((key, _)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"');
        let Some(section) = section else {
            found.push(UnknownKey {
                name: key.to_owned(),
                line: index + 1,
                suggestion: None,
            });
            continue;
        };
        if !known_keys.contains(&key) {
            found.push(UnknownKey {
                name: format!("[{section}] {key}"),
                line: index + 1,
                suggestion: closest(key, known_keys.iter().copied())
                    .map(|k| format!("[{section}] {k}")),
            });
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_the_framework_does_not_read_are_named() {
        let text = "[server]\nport = 3030\n\n[app]\nsecret = \"x\"\nminify_html = true\nsesion_days = 7\n\n[cache]\nttl = 5\n";
        let found = unknown_keys(text);
        assert_eq!(found.len(), 3, "{found:?}");
        assert_eq!(found[0].name, "[app] minify_html");
        assert_eq!(found[0].line, 6);
        assert_eq!(found[0].suggestion, None);
        assert_eq!(found[1].name, "[app] sesion_days");
        assert_eq!(found[1].suggestion.as_deref(), Some("[app] session_days"));
        assert_eq!(found[2].name, "[cache]");
        assert!(
            found[0].message().contains("не діє"),
            "{}",
            found[0].message()
        );
    }

    #[test]
    fn every_documented_key_is_accepted() {
        let text = "[server]\nport = 1\ntrust_proxy = true\n[db]\ndriver = \"sqlite\"\nurl = \"x\"\n\
            [app]\nsecret = \"s\"\ncsrf = true\nsession_cookie = \"c\"\nsession_days = 1\n\
            session_secure = true\ntz_offset = \"+03:00\"\nhttp_timeout = 5\nlocale = \"uk\"\n\
            [mail]\nfrom = \"a\"\nsmtp_host = \"h\"\nsmtp_port = 25\nsmtp_user = \"u\"\nsmtp_pass = \"p\"\n\
            [api]\ncors = \"*\"\ntitle = \"t\"\nversion = \"1\"\nopenapi = true\n";
        assert_eq!(unknown_keys(text), vec![]);
    }

    #[test]
    fn the_projects_in_this_repository_have_no_unknown_keys() {
        // Приклади мають бути взірцем: якщо тут щось знайшлось, або приклад
        // застарів, або KNOWN забули доповнити новим параметром.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for project in [
            "examples/demo",
            "examples/cookbook",
            "crates/rhaix-server/tests/fixture",
        ] {
            let text =
                std::fs::read_to_string(root.join(project).join("rhaix.toml")).unwrap_or_default();
            assert_eq!(unknown_keys(&text), vec![], "{project}");
        }
    }
}
