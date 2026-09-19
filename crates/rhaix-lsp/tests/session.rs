//! Наскрізна сесія LSP: справжній процес, справжній протокол по stdio.
//!
//! Юніт-тести перевіряють шматки; цей — що сервер справді розмовляє з
//! редактором: відповідає на `initialize`, шле діагностику на відкритий файл,
//! знаходить компонент і доповнює.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Session {
    fn start() -> Self {
        let exe = env!("CARGO_BIN_EXE_rhaix-lsp");
        let mut child = Command::new(exe)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("сервер має запуститись");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, message: Value) {
        let body = message.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).expect("надіслано");
        self.stdin.flush().expect("flush");
    }

    fn receive(&mut self) -> Value {
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).expect("заголовок");
            assert!(read > 0, "сервер закрив потік");
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break;
            }
            if let Some(value) = trimmed.strip_prefix("Content-Length:") {
                length = value.trim().parse().expect("довжина");
            }
        }
        let mut body = vec![0u8; length];
        std::io::Read::read_exact(&mut self.stdout, &mut body).expect("тіло");
        serde_json::from_slice(&body).expect("JSON")
    }

    fn initialize(&mut self) -> Value {
        self.send(json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "processId": null, "rootUri": null, "capabilities": {} }
        }));
        self.receive()
    }

    fn open(&mut self, path: &Path, text: &str) -> Value {
        self.send(json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": uri(path), "languageId": "rhx", "version": 1, "text": text
            }}
        }));
        self.receive()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn uri(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    let text = text.trim_start_matches("//?/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../rhaix-server/tests/fixture")
}

#[test]
fn server_announces_what_it_can_do() {
    let mut session = Session::start();
    let reply = session.initialize();

    let caps = &reply["result"]["capabilities"];
    assert_eq!(caps["textDocumentSync"], 1);
    assert_eq!(caps["definitionProvider"], true);
    assert!(caps["completionProvider"].is_object(), "{reply}");
}

#[test]
fn a_broken_file_gets_a_diagnostic_at_the_right_place() {
    let mut session = Session::start();
    session.initialize();

    let file = fixture().join("pages/scratch.rhx");
    // Невідомий компонент у другому рядку.
    let text = "<p>Привіт</p>\n<NoSuchComponent />\n";
    let note = session.open(&file, text);

    assert_eq!(note["method"], "textDocument/publishDiagnostics");
    let diagnostics = note["params"]["diagnostics"].as_array().expect("масив");
    assert_eq!(diagnostics.len(), 1, "{note}");

    let first = &diagnostics[0];
    assert_eq!(first["severity"], 1);
    assert_eq!(first["source"], "rhaix");
    assert!(
        first["message"]
            .as_str()
            .unwrap()
            .contains("NoSuchComponent"),
        "{first}"
    );
    // Помилка саме в другому рядку (0-based), а не на початку файлу.
    assert_eq!(first["range"]["start"]["line"], 1, "{first}");
}

#[test]
fn a_correct_file_gets_an_empty_list() {
    let mut session = Session::start();
    session.initialize();

    let file = fixture().join("pages/scratch.rhx");
    let note = session.open(&file, "<p>{{ 2 + 2 }}</p>");

    let diagnostics = note["params"]["diagnostics"].as_array().expect("масив");
    assert!(diagnostics.is_empty(), "{note}");
}

#[test]
fn diagnostics_survive_cyrillic_positions() {
    // Кирилиця до помилки: якщо рахувати байти замість символів UTF-16,
    // підкреслення в редакторі поїде вправо.
    let mut session = Session::start();
    session.initialize();

    let file = fixture().join("pages/scratch.rhx");
    let text = "<p>Привіт</p><NoSuchComponent />";
    let note = session.open(&file, text);

    let first = &note["params"]["diagnostics"][0];
    let character = first["range"]["start"]["character"]
        .as_u64()
        .expect("колонка");
    // Діагностика вказує на **ім'я** компонента, тобто одразу після `<`.
    // `<p>Привіт</p><` — 14 символів UTF-16, але 20 байтів. Саме тут мовні
    // сервери промахуються на кирилиці, тому межу перевіряємо явно.
    assert_eq!(character, 14, "{first}");
    assert_ne!(character, 20, "позицію пораховано в байтах, а не в UTF-16");
}

#[test]
fn go_to_definition_opens_the_component() {
    let mut session = Session::start();
    session.initialize();

    let file = fixture().join("pages/scratch.rhx");
    let text = "<Greeting name=\"світ\" />";
    session.open(&file, text);

    session.send(json!({
        "jsonrpc": "2.0", "id": 2, "method": "textDocument/definition",
        "params": {
            "textDocument": { "uri": uri(&file) },
            "position": { "line": 0, "character": 3 }
        }
    }));
    let reply = session.receive();
    let target = reply["result"]["uri"].as_str().expect("посилання");
    assert!(target.ends_with("components/Greeting.rhx"), "{reply}");
}

#[test]
fn completion_offers_components_and_directives() {
    let mut session = Session::start();
    session.initialize();

    let file = fixture().join("pages/scratch.rhx");
    session.open(&file, "<Gre");

    // Після `<` з великої літери — компоненти проєкту.
    session.send(json!({
        "jsonrpc": "2.0", "id": 3, "method": "textDocument/completion",
        "params": {
            "textDocument": { "uri": uri(&file) },
            "position": { "line": 0, "character": 4 }
        }
    }));
    let reply = session.receive();
    let labels: Vec<&str> = reply["result"]
        .as_array()
        .expect("масив")
        .iter()
        .map(|item| item["label"].as_str().unwrap_or(""))
        .collect();
    assert!(labels.contains(&"Greeting"), "{labels:?}");
    assert!(labels.contains(&"Ui.Card"), "{labels:?}");

    // Після `@` усередині тега — директиви.
    session.open(&file, "<li @");
    session.send(json!({
        "jsonrpc": "2.0", "id": 4, "method": "textDocument/completion",
        "params": {
            "textDocument": { "uri": uri(&file) },
            "position": { "line": 0, "character": 5 }
        }
    }));
    let reply = session.receive();
    let labels: Vec<&str> = reply["result"]
        .as_array()
        .expect("масив")
        .iter()
        .map(|item| item["label"].as_str().unwrap_or(""))
        .collect();
    assert!(labels.contains(&"@if"), "{labels:?}");
    assert!(labels.contains(&"@for"), "{labels:?}");
}

#[test]
fn shutdown_and_exit_end_the_process() {
    let mut session = Session::start();
    session.initialize();

    session.send(json!({ "jsonrpc": "2.0", "id": 9, "method": "shutdown" }));
    let reply = session.receive();
    assert_eq!(reply["id"], 9);

    session.send(json!({ "jsonrpc": "2.0", "method": "exit" }));
    let status = session.child.wait().expect("процес має завершитись");
    assert!(status.success(), "{status:?}");
}
