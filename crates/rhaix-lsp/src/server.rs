//! Обробка запитів LSP.
//!
//! Сервер синхронний і однопотоковий: компіляція `.rhx` теж синхронна, а файл
//! у редакторі один. Складнощів з паралелізмом тут просто немає.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::{project, rpc};

/// Відкриті документи: шлях → поточний текст із редактора.
#[derive(Default)]
pub struct Server {
    documents: HashMap<PathBuf, String>,
}

impl Server {
    pub fn new() -> Self {
        Self::default()
    }

    /// Головний цикл: читаємо повідомлення, відповідаємо, доки не закриють потік.
    pub fn run(&mut self, input: &mut impl BufRead, output: &mut impl Write) {
        while let Some(message) = rpc::read_message(input) {
            let method = message.get("method").and_then(Value::as_str).unwrap_or("");
            let id = message.get("id").cloned();
            let params = message.get("params").cloned().unwrap_or(Value::Null);

            match method {
                "initialize" => {
                    if let Some(id) = id {
                        rpc::write_message(output, &rpc::response(id, capabilities()));
                    }
                }
                "shutdown" => {
                    if let Some(id) = id {
                        rpc::write_message(output, &rpc::response(id, Value::Null));
                    }
                }
                "exit" => return,
                "textDocument/didOpen" | "textDocument/didChange" | "textDocument/didSave" => {
                    self.sync(&params);
                    if let Some((path, _)) = self.document_of(&params) {
                        let report = self.diagnose(&path);
                        rpc::write_message(output, &report);
                    }
                }
                "textDocument/didClose" => {
                    if let Some((path, _)) = self.document_of(&params) {
                        self.documents.remove(&path);
                    }
                }
                "textDocument/definition" => {
                    if let Some(id) = id {
                        let result = self.definition(&params).unwrap_or(Value::Null);
                        rpc::write_message(output, &rpc::response(id, result));
                    }
                }
                "textDocument/completion" => {
                    if let Some(id) = id {
                        let items = self.completion(&params);
                        rpc::write_message(output, &rpc::response(id, json!(items)));
                    }
                }
                // Решта методів нам не потрібна, але на запит треба відповісти —
                // інакше клієнт чекатиме вічно.
                _ => {
                    if let Some(id) = id {
                        rpc::write_message(output, &rpc::response(id, Value::Null));
                    }
                }
            }
        }
    }

    // ------------------------------------------------------ документи

    fn sync(&mut self, params: &Value) {
        let Some((path, _)) = self.document_of(params) else {
            return;
        };
        // didOpen кладе текст у `textDocument.text`, didChange — у
        // `contentChanges[0].text` (ми просимо повну синхронізацію).
        let text = params
            .pointer("/textDocument/text")
            .and_then(Value::as_str)
            .or_else(|| {
                params
                    .pointer("/contentChanges/0/text")
                    .and_then(Value::as_str)
            });

        match text {
            Some(text) => {
                self.documents.insert(path, text.to_owned());
            }
            None => {
                // didSave без тексту — перечитуємо з диска.
                if let Ok(text) = std::fs::read_to_string(&path) {
                    self.documents.insert(path, text);
                }
            }
        }
    }

    fn document_of(&self, params: &Value) -> Option<(PathBuf, String)> {
        let uri = params
            .pointer("/textDocument/uri")
            .and_then(Value::as_str)?;
        let path = uri_to_path(uri)?;
        let text = self.documents.get(&path).cloned().unwrap_or_default();
        Some((path, text))
    }

    // ------------------------------------------------------ діагностика

    /// Скомпілювати документ і зібрати `publishDiagnostics`.
    fn diagnose(&self, path: &Path) -> Value {
        let text = self.documents.get(path).cloned().unwrap_or_default();
        let uri = path_to_uri(path);

        let Some(root) = project::find_root(path) else {
            // Файл поза проєктом: мовчимо, а не сиплемо помилками.
            return rpc::notification(
                "textDocument/publishDiagnostics",
                json!({ "uri": uri, "diagnostics": [] }),
            );
        };

        let diagnostics = match project::compile(&root, path, &text) {
            Ok(_) => Vec::new(),
            Err(diagnostic) => {
                // Помилка може бути в іншому файлі (компонент), тоді показувати
                // її тут зайве — але сказати, що не так, треба. Ставимо на
                // початок файлу з поясненням, у якому файлі проблема.
                let same_file = diagnostic
                    .source
                    .as_ref()
                    .map(|source| source.path() == path)
                    .unwrap_or(true);

                let range = if same_file {
                    rpc::range(&text, diagnostic.span.start, diagnostic.span.end)
                } else {
                    rpc::range(&text, 0, 0)
                };

                let mut message = diagnostic.message.clone();
                if let Some(hint) = &diagnostic.hint {
                    message.push('\n');
                    message.push_str(hint);
                }
                if !same_file {
                    if let Some(source) = &diagnostic.source {
                        message = format!("{}: {message}", source.path().display());
                    }
                }

                vec![json!({
                    "range": range,
                    "severity": 1, // Error
                    "source": "rhaix",
                    "message": message,
                })]
            }
        };

        rpc::notification(
            "textDocument/publishDiagnostics",
            json!({ "uri": uri, "diagnostics": diagnostics }),
        )
    }

    // ------------------------------------------------------ перехід

    /// `<TodoItem/>` під курсором → файл компонента.
    fn definition(&self, params: &Value) -> Option<Value> {
        let (path, text) = self.document_of(params)?;
        let line = params.pointer("/position/line")?.as_u64()? as u32;
        let character = params.pointer("/position/character")?.as_u64()? as u32;
        let offset = rpc::position_to_offset(&text, line, character);

        let name = component_at(&text, offset)?;
        let root = project::find_root(&path)?;
        let target = project::component_path(&root, &name);
        if !target.is_file() {
            return None;
        }

        Some(json!({
            "uri": path_to_uri(&target),
            "range": { "start": { "line": 0, "character": 0 },
                       "end": { "line": 0, "character": 0 } },
        }))
    }

    // ------------------------------------------------------ доповнення

    fn completion(&self, params: &Value) -> Vec<Value> {
        let Some((path, text)) = self.document_of(params) else {
            return Vec::new();
        };
        let (Some(line), Some(character)) = (
            params.pointer("/position/line").and_then(Value::as_u64),
            params
                .pointer("/position/character")
                .and_then(Value::as_u64),
        ) else {
            return Vec::new();
        };
        let offset = rpc::position_to_offset(&text, line as u32, character as u32);
        let before = &text[..offset];

        match context_at(before) {
            Context::Component => project::find_root(&path)
                .map(|root| project::component_names(&root))
                .unwrap_or_default()
                .into_iter()
                .map(|name| item(&name, 7, "компонент"))
                .collect(),
            Context::Directive => DIRECTIVES
                .iter()
                .map(|(name, detail)| item(name, 14, detail))
                .collect(),
            Context::Expression => GLOBALS
                .iter()
                .map(|(name, detail)| item(name, 6, detail))
                .chain(BUILTINS.iter().map(|(name, detail)| item(name, 3, detail)))
                .collect(),
            Context::None => Vec::new(),
        }
    }
}

fn item(label: &str, kind: u8, detail: &str) -> Value {
    json!({ "label": label, "kind": kind, "detail": detail })
}

/// Що доповнювати в цьому місці.
#[derive(Debug, PartialEq, Eq)]
pub enum Context {
    /// Після `<` з великої літери — компонент.
    Component,
    /// Після `@` всередині тега — директива.
    Directive,
    /// Усередині `{{ }}`, `={...}` або frontmatter — вираз.
    Expression,
    None,
}

/// Визначити контекст за текстом **до** курсора.
pub fn context_at(before: &str) -> Context {
    // Frontmatter: між першим і другим `---`.
    if in_frontmatter(before) {
        return Context::Expression;
    }
    // Незакрита інтерполяція `{{`.
    if let (Some(open), close) = (before.rfind("{{"), before.rfind("}}")) {
        if close.map(|c| c < open).unwrap_or(true) {
            return Context::Expression;
        }
    }

    // Усередині тега: останній `<` пізніший за останній `>`.
    let open = before.rfind('<');
    let close = before.rfind('>');
    let inside_tag = match (open, close) {
        (Some(o), Some(c)) => o > c,
        (Some(_), None) => true,
        _ => false,
    };

    if inside_tag {
        let tag = &before[open.unwrap()..];
        // `<Ab` — ім'я компонента ще пишуть.
        let after_bracket = &tag[1..];
        if after_bracket
            .chars()
            .next()
            .map(|c| c.is_ascii_uppercase())
            .unwrap_or(true)
            && !after_bracket.contains(char::is_whitespace)
        {
            return Context::Component;
        }
        // Значення атрибута у фігурних дужках — вираз.
        if let Some(brace) = tag.rfind('{') {
            if !tag[brace..].contains('}') {
                return Context::Expression;
            }
        }
        if tag.ends_with('@') || last_word(tag).starts_with('@') {
            return Context::Directive;
        }
    }
    Context::None
}

fn in_frontmatter(before: &str) -> bool {
    if !before.starts_with("---") {
        return false;
    }
    // Другий `---` на власному рядку закриває блок.
    let rest = &before[3..];
    !rest.lines().any(|line| line.trim() == "---")
}

fn last_word(text: &str) -> &str {
    text.rsplit(|c: char| c.is_whitespace())
        .next()
        .unwrap_or("")
}

/// Ім'я компонента, у теге якого стоїть курсор.
pub fn component_at(text: &str, offset: usize) -> Option<String> {
    let offset = offset.min(text.len());
    // Ліва межа тега.
    let open = text[..offset].rfind('<')?;
    // Між `<` і курсором не має бути `>`: інакше ми вже поза тегом.
    if text[open..offset].contains('>') {
        return None;
    }

    let after = &text[open + 1..];
    let name: String = after
        .trim_start_matches('/')
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '.')
        .collect();

    // Компонент — це тег із великої літери.
    if name.chars().next()?.is_ascii_uppercase() {
        Some(name)
    } else {
        None
    }
}

fn capabilities() -> Value {
    json!({
        "capabilities": {
            // Повна синхронізація: файли `.rhx` маленькі, а інкрементальна
            // склейка — зайве джерело розбіжностей із буфером редактора.
            "textDocumentSync": 1,
            "definitionProvider": true,
            "completionProvider": { "triggerCharacters": ["<", "@", "."] },
        },
        "serverInfo": { "name": "rhaix-lsp", "version": env!("CARGO_PKG_VERSION") },
    })
}

const DIRECTIVES: [(&str, &str); 11] = [
    ("@if", "умова"),
    ("@else-if", "інакше якщо"),
    ("@else", "інакше"),
    ("@for", "цикл: `x in coll`"),
    ("@key", "ключ елемента циклу"),
    ("@class", "класи з мапи/масиву"),
    ("@style", "стилі з мапи"),
    ("@attr", "атрибути з мапи"),
    ("@html", "вміст без екранування"),
    ("@text", "вміст із екрануванням"),
    ("@oob", "out-of-band своп"),
];

const GLOBALS: [(&str, &str); 14] = [
    ("req", "запит"),
    ("res", "відповідь"),
    ("hx", "HTMX: тости, тригери, редіректи"),
    ("db", "база даних"),
    ("session", "сесія (підписаний cookie)"),
    ("csrf", "csrf.token"),
    ("http", "виклик чужого API"),
    ("mail", "надсилання пошти"),
    ("state", "процесне сховище; state.allow — ліміт спроб"),
    ("live", "live.send(тема) — оновити відкриті сторінки"),
    ("page", "спільна мапа сторінки й layout"),
    ("log", "log.info/warn/error"),
    ("props", "значення, передані компоненту"),
    ("slots", "slots.has(name)"),
];

const BUILTINS: [(&str, &str); 20] = [
    ("t", "переклад: t(\"ключ\")"),
    ("validate", "перевірка форми"),
    ("paginate", "арифметика сторінок"),
    ("csv", "таблиця в CSV: csv(rows, #{ columns: [...] })"),
    ("url", "посилання з параметрами"),
    ("date", "дата за шаблоном"),
    ("datetime", "дата й час"),
    ("now", "секунди від епохи"),
    ("money", "сума з роздільником тисяч"),
    ("slug", "транслітерація в адресу"),
    ("cut", "обрізати по межі слова"),
    ("uuid", "UUID v4"),
    ("random_id", "випадковий ідентифікатор"),
    ("hash_password", "Argon2id"),
    ("verify_password", "перевірка пароля"),
    ("json", "значення для <script>"),
    ("json_encode", "JSON-рядок"),
    ("json_decode", "розібрати JSON-рядок"),
    ("raw", "вивід без екранування"),
    ("markdown", "CommonMark без сирого HTML"),
];

/// `file:///C:/x/y.rhx` → шлях.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    let decoded = percent_decode(rest);
    // На Windows шлях має вигляд `C:/x`, на Unix — `/x`.
    if decoded.chars().nth(1) == Some(':') {
        Some(PathBuf::from(decoded))
    } else {
        Some(PathBuf::from(format!("/{decoded}")))
    }
}

pub fn path_to_uri(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    let text = text.trim_start_matches("//?/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_under_the_cursor_is_found() {
        let text = "<ul>\n  <TodoItem todo={t} />\n</ul>";
        let offset = text.find("TodoItem").unwrap() + 3;
        assert_eq!(component_at(text, offset).as_deref(), Some("TodoItem"));

        // Крапка в імені — вкладена тека.
        let text = "<Ui.Card>";
        assert_eq!(component_at(text, 5).as_deref(), Some("Ui.Card"));

        // Звичайний тег компонентом не є.
        let text = "<div class=\"x\">";
        assert_eq!(component_at(text, 3), None);
    }

    #[test]
    fn completion_context_is_detected() {
        assert_eq!(context_at("<ul>\n  <Tod"), Context::Component);
        assert_eq!(context_at("<li @"), Context::Directive);
        assert_eq!(context_at("<li @i"), Context::Directive);
        assert_eq!(context_at("<p>{{ req."), Context::Expression);
        assert_eq!(context_at("<li @if={todos."), Context::Expression);
        assert_eq!(context_at("---\nlet x = "), Context::Expression);
        // Після закритої інтерполяції — уже звичайний текст.
        assert_eq!(context_at("<p>{{ x }} далі"), Context::None);
        // Після закритого frontmatter — теж.
        assert_eq!(context_at("---\nlet x = 1;\n---\n<p>текст"), Context::None);
    }

    #[test]
    fn uri_and_path_round_trip() {
        let uri = "file:///C:/Users/x/app/pages/index.rhx";
        let path = uri_to_path(uri).expect("шлях");
        assert_eq!(path_to_uri(&path), uri);

        // Пробіли в шляху приходять закодованими.
        let path = uri_to_path("file:///C:/my%20app/x.rhx").expect("шлях");
        assert!(path.display().to_string().contains("my app"), "{path:?}");
    }
}
