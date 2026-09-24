//! Опис `api/` у форматі OpenAPI 3.1 — з тих самих файлів, без анотацій.
//!
//! Рукописна специфікація розходиться з кодом із першого ж коміту. Тому опис
//! виводиться з того, що вже написано в `api/*.rhx`:
//!
//! - шлях і параметри шляху — з імені файлу (`api/orders/[id].rhx`);
//! - методи — з `req.method == "POST"` (і охорон `req.method != "POST"`);
//! - query-параметри та їхні типи — з `req.query("q")`, `req.query_int("page")`;
//! - тіло JSON — з `req.json()`, а його поля й обмеження — з `validate(...)`;
//! - коди відповідей — з `res.status(...)`;
//! - опис — з коментаря на початку frontmatter;
//! - bearer-токен — якщо `middleware.rhx` читає заголовок `Authorization`.
//!
//! Це евристика, і вона чесна щодо цього: що не вдалося вивести, того в
//! описі просто немає, а не вигадано.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};

use crate::{scan_api, Config};

/// Зібрати специфікацію для всього `api/` проєкту.
pub fn openapi(config: &Config) -> anyhow::Result<Value> {
    let routes = scan_api(config.files.as_ref(), &config.api_dir())?;

    // Middleware стоїть перед кожним маршрутом `api/`: його відмови (401 без
    // токена, 429 за лімітом) — теж можливі відповіді кожної операції.
    let middleware_text = config
        .files
        .read_text(&config.middleware_path())
        .unwrap_or_default();
    let middleware = frontmatter(&middleware_text);
    let guards_api = middleware.contains("/api");
    let bearer = guards_api
        && middleware
            .to_ascii_lowercase()
            .contains("\"authorization\"");
    let shared_codes = if guards_api {
        status_codes(api_block(middleware))
    } else {
        BTreeSet::new()
    };

    let mut paths = Map::new();
    for route in &routes {
        let text = config.files.read_text(&route.file).unwrap_or_default();
        let script = frontmatter(&text);
        let path = openapi_path(&route.pattern);
        let operations = describe(&route.pattern, script, &shared_codes);
        paths.insert(path, Value::Object(operations));
    }

    let mut spec = json!({
        "openapi": "3.1.0",
        "info": {
            "title": config.app.api_title.clone(),
            "version": config.app.api_version.clone(),
        },
        "paths": paths,
    });
    if bearer {
        spec["components"] = json!({
            "securitySchemes": { "bearer": { "type": "http", "scheme": "bearer" } }
        });
        spec["security"] = json!([{ "bearer": [] }]);
    }
    Ok(spec)
}

/// Частина middleware, що стосується `api/`: блок після
/// `req.path.starts_with("/api…")`. Решта файлу охороняє сторінки, і її 403
/// чи редіректи до API не мають стосунку.
fn api_block(middleware: &str) -> &str {
    let Some(at) = middleware.find("starts_with(\"/api") else {
        return middleware;
    };
    let Some(open) = middleware[at..].find('{').map(|o| at + o) else {
        return middleware;
    };
    &middleware[open..matching_brace(middleware, open)]
}

/// `/api/orders/{id}` лишається, `{*rest}` (axum) → `{rest}` (OpenAPI).
fn openapi_path(pattern: &str) -> String {
    pattern.replace("{*", "{")
}

/// Код між першими двома рядками `---`. Файл без frontmatter — порожній.
fn frontmatter(text: &str) -> &str {
    let text = text.trim_start_matches('\u{feff}');
    let Some(rest) = text.strip_prefix("---") else {
        return "";
    };
    let rest = rest.trim_start_matches(['\r', '\n']);
    match rest.find("\n---") {
        Some(end) => &rest[..end],
        None => rest,
    }
}

/// Усі операції одного файлу.
///
/// Кожен метод описується за **своїм** кодом: блок `if req.method == "POST"
/// { … }` — це POST, решта файлу — GET. Інакше GET «повертав» би 201 і 422,
/// які насправді живуть лише в гілці створення.
fn describe(pattern: &str, script: &str, shared_codes: &BTreeSet<u16>) -> Map<String, Value> {
    let (summary, description) = doc_comment(script);

    let mut operations = Map::new();
    for (method, code) in sections(script) {
        let parameters = parameters(pattern, &code);
        let body = request_body(&code);
        let responses = responses(&code, shared_codes);
        let mut operation = Map::new();
        if let Some(summary) = &summary {
            operation.insert("summary".into(), json!(summary));
        }
        if let Some(description) = &description {
            operation.insert("description".into(), json!(description));
        }
        if !parameters.is_empty() {
            // Query-параметри має лише GET: на POST їх зазвичай читають з тіла.
            let list: Vec<&Value> = parameters
                .iter()
                .filter(|p| method == "get" || p["in"] == "path")
                .collect();
            if !list.is_empty() {
                operation.insert("parameters".into(), json!(list));
            }
        }
        if method != "get" && method != "delete" {
            if let Some(body) = &body {
                operation.insert("requestBody".into(), body.clone());
            }
        }
        operation.insert("responses".into(), responses.clone());
        operations.insert(method, Value::Object(operation));
    }
    operations
}

/// Код кожного методу: `(метод, його код)`.
fn sections(script: &str) -> Vec<(String, String)> {
    let known = ["get", "post", "put", "patch", "delete"];

    // `if req.method != "POST" { res.status(405); … }` — файл лише для POST, і
    // весь його код — цього методу.
    let guarded: Vec<String> = literals_after(script, "req.method !=")
        .into_iter()
        .map(|m| m.to_ascii_lowercase())
        .filter(|m| known.contains(&m.as_str()))
        .collect();
    if !guarded.is_empty() {
        let mut methods: Vec<String> = Vec::new();
        for method in known {
            if guarded.iter().any(|m| m == method) {
                methods.push(method.to_owned());
            }
        }
        return methods
            .into_iter()
            .map(|m| (m, script.to_owned()))
            .collect();
    }

    // Блоки `if req.method == "X" { … }`: їхній код — методу X, решта — GET.
    let mut blocks: BTreeMap<String, String> = BTreeMap::new();
    let mut cut: Vec<(usize, usize)> = Vec::new();
    let marker = "req.method ==";
    let mut from = 0;
    while let Some(found) = script[from..].find(marker) {
        let at = from + found + marker.len();
        from = at;
        let tail = script[at..].trim_start();
        let Some((method, _)) = tail.strip_prefix('"').and_then(|t| t.split_once('"')) else {
            continue;
        };
        let method = method.to_ascii_lowercase();
        let Some(open) = script[at..].find('{').map(|o| at + o) else {
            continue;
        };
        let close = matching_brace(script, open);
        blocks
            .entry(method)
            .or_default()
            .push_str(&script[open..close]);
        cut.push((open, close));
        from = close;
    }
    let mut rest = String::new();
    let mut last = 0;
    for (open, close) in &cut {
        rest.push_str(&script[last..*open]);
        last = *close;
    }
    rest.push_str(&script[last..]);

    let mut sections = Vec::new();
    for method in known {
        let code = match (method, blocks.get(method)) {
            ("get", Some(block)) => format!("{block}\n{rest}"),
            ("get", None) => rest.clone(),
            (_, Some(block)) => block.clone(),
            (_, None) => continue,
        };
        sections.push((method.to_owned(), code));
    }
    sections
}

/// Позиція одразу за дужкою, що закриває `{` на позиції `open`.
fn matching_brace(text: &str, open: usize) -> usize {
    let mut depth = 0;
    let mut quoted = false;
    for (offset, ch) in text[open..].char_indices() {
        match ch {
            '"' => quoted = !quoted,
            '{' if !quoted => depth += 1,
            '}' if !quoted => {
                depth -= 1;
                if depth == 0 {
                    return open + offset + 1;
                }
            }
            _ => {}
        }
    }
    text.len()
}

/// Коментар на самому початку frontmatter: перше речення — `summary`, усе
/// разом — `description`.
fn doc_comment(script: &str) -> (Option<String>, Option<String>) {
    let mut lines = Vec::new();
    for line in script.lines() {
        let line = line.trim();
        if let Some(text) = line.strip_prefix("//") {
            lines.push(text.trim().to_owned());
        } else if line.is_empty() && lines.is_empty() {
            continue;
        } else {
            break;
        }
    }
    // Порожні рядки коментаря на краях не потрібні.
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return (None, None);
    }
    let description = lines.join("\n").trim().to_owned();
    // Перший абзац одним рядком, до кінця першого речення: рядок коментаря
    // часто обривається посеред думки.
    let paragraph: Vec<&str> = lines
        .iter()
        .take_while(|l| !l.is_empty())
        .map(String::as_str)
        .collect();
    let joined = paragraph.join(" ");
    let joined = joined.trim_start_matches("ЗАДАЧА:").trim();
    let summary = match joined.find(". ") {
        Some(end) => &joined[..=end],
        None => joined,
    };
    (Some(summary.trim().to_owned()), Some(description))
}

/// Рядкові літерали, що стоять одразу після `prefix` (з пробілами між).
fn literals_after(script: &str, prefix: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = script;
    while let Some(at) = rest.find(prefix) {
        rest = &rest[at + prefix.len()..];
        let tail = rest.trim_start();
        if let Some(value) = tail.strip_prefix('"').and_then(|t| t.split_once('"')) {
            found.push(value.0.to_owned());
        }
    }
    found
}

fn parameters(pattern: &str, script: &str) -> Vec<Value> {
    let mut list = Vec::new();
    for segment in pattern.split('/') {
        if let Some(name) = segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            let name = name.trim_start_matches('*');
            list.push(json!({
                "name": name, "in": "path", "required": true,
                "schema": { "type": "string" },
            }));
        }
    }
    let mut seen = BTreeSet::new();
    for (call, kind) in [
        ("req.query_int(", "integer"),
        ("req.query_float(", "number"),
        ("req.query_bool(", "boolean"),
        ("req.query(", "string"),
    ] {
        for name in literals_after(script, call) {
            if seen.insert(name.clone()) {
                list.push(json!({
                    "name": name, "in": "query", "required": false,
                    "schema": { "type": kind },
                }));
            }
        }
    }
    list
}

fn request_body(script: &str) -> Option<Value> {
    if script.contains("req.json()") {
        let schema = validate_schema(script).unwrap_or_else(|| json!({ "type": "object" }));
        return Some(json!({
            "required": true,
            "content": { "application/json": { "schema": schema } },
        }));
    }
    if script.contains("req.form(") || script.contains("req.all_form()") {
        return Some(json!({
            "content": { "application/x-www-form-urlencoded": { "schema": { "type": "object" } } },
        }));
    }
    None
}

/// Схема тіла з першого `validate(x, #{ поле: "правила", … })`.
///
/// Правила форм уже кажуть усе, що треба схемі: `required`, тип, межі. Тож
/// писати це вдруге в специфікації не доводиться.
fn validate_schema(script: &str) -> Option<Value> {
    let at = script.find("validate(")?;
    let open = at + script[at..].find("#{")? + 2;
    let mut depth = 1;
    let mut end = open;
    for (offset, ch) in script[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = open + offset;
                    break;
                }
            }
            _ => {}
        }
    }
    let body = &script[open..end];

    let mut properties = Map::new();
    let mut required = Vec::new();
    for entry in split_outside_quotes(body) {
        let Some((name, rules)) = entry.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let Some(rules) = rules
            .trim()
            .strip_prefix('"')
            .and_then(|r| r.split_once('"'))
        else {
            continue;
        };
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let (schema, is_required) = rule_schema(rules.0);
        if is_required {
            required.push(json!(name));
        }
        properties.insert(name.to_owned(), schema);
    }
    if properties.is_empty() {
        return None;
    }
    let mut schema = json!({ "type": "object", "properties": properties });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    Some(schema)
}

/// Поділити на записи за комами поза лапками: у `"in:new,paid"` кома — частина
/// правила, а не роздільник.
fn split_outside_quotes(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    for (index, ch) in text.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                parts.push(&text[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// Правила `validate()` → JSON Schema одного поля.
fn rule_schema(rules: &str) -> (Value, bool) {
    let mut schema = Map::new();
    let mut required = false;
    let mut kind = "string";
    let mut bounds: BTreeMap<&str, f64> = BTreeMap::new();
    for rule in rules.split('|') {
        let (name, arg) = rule.split_once(':').unwrap_or((rule, ""));
        match name.trim() {
            "required" => required = true,
            "int" => kind = "integer",
            "number" => kind = "number",
            "bool" => kind = "boolean",
            "email" => {
                schema.insert("format".into(), json!("email"));
            }
            "url" => {
                schema.insert("format".into(), json!("uri"));
            }
            "date" => {
                schema.insert("format".into(), json!("date"));
            }
            "in" => {
                let values: Vec<&str> = arg.split(',').map(str::trim).collect();
                schema.insert("enum".into(), json!(values));
            }
            "min" | "max" | "len" => {
                if let Ok(value) = arg.trim().parse::<f64>() {
                    bounds.insert(name.trim(), value);
                }
            }
            "between" => {
                if let Some((low, high)) = arg.split_once(',') {
                    if let (Ok(low), Ok(high)) = (low.trim().parse(), high.trim().parse()) {
                        bounds.insert("min", low);
                        bounds.insert("max", high);
                    }
                }
            }
            _ => {}
        }
    }
    schema.insert("type".into(), json!(kind));
    // Як і в `validate()`: для чисел межа — значення, для рядків — довжина.
    let numeric = kind == "integer" || kind == "number";
    for (name, value) in bounds {
        let key = match (name, numeric) {
            ("min", true) => "minimum",
            ("max", true) => "maximum",
            ("min", false) => "minLength",
            ("max", false) => "maxLength",
            ("len", false) => {
                schema.insert("minLength".into(), json!(value as i64));
                "maxLength"
            }
            _ => continue,
        };
        if numeric {
            schema.insert(key.into(), json!(value));
        } else {
            schema.insert(key.into(), json!(value as i64));
        }
    }
    (Value::Object(schema), required)
}

fn status_codes(script: &str) -> BTreeSet<u16> {
    let mut codes: BTreeSet<u16> = BTreeSet::new();
    let mut rest = script;
    while let Some(at) = rest.find("res.status(") {
        rest = &rest[at + "res.status(".len()..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(code) = digits.parse::<u16>() {
            if (100..600).contains(&code) {
                codes.insert(code);
            }
        }
    }
    codes
}

fn responses(script: &str, shared: &BTreeSet<u16>) -> Value {
    let mut codes = status_codes(script);
    // Шлях без явного успішного статусу відповідає 200.
    if !codes.iter().any(|c| (200..300).contains(c)) {
        codes.insert(200);
    }
    codes.extend(shared.iter().copied());
    let mut map = Map::new();
    for code in codes {
        let content = if code == 204 {
            json!({ "description": reason(code) })
        } else {
            json!({
                "description": reason(code),
                "content": { "application/json": { "schema": {} } },
            })
        };
        map.insert(code.to_string(), content);
    }
    Value::Object(map)
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        422 => "Unprocessable Content",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Response",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORDERS: &str = r#"---
// Колекція замовлень.
// GET — сторінка списку, POST — створення.

if req.method == "POST" {
    let sent = req.json();
    let errors = validate(sent, #{
        customer: "required|min:2",
        email:    "required|email",
        amount:   "required|number|min:0.01",
        status:   "in:new,paid",
        age:      "int|between:18,120",
    });
    if !errors.is_empty() { res.status(422); return #{ errors: errors }; }
    res.status(201);
    return #{ id: 1 };
}
let p = paginate(10, 20, req.query_int("page") ?? 1);
let q = req.query("q");
return #{ data: [] };
---
"#;

    #[test]
    fn methods_parameters_body_and_codes_come_from_the_script() {
        let operations = describe("/api/orders", frontmatter(ORDERS), &BTreeSet::from([401]));
        assert_eq!(
            operations.keys().collect::<Vec<_>>(),
            vec!["get", "post"],
            "код поза if — це GET"
        );

        let get = &operations["get"];
        assert_eq!(get["summary"], "Колекція замовлень.");
        let params = get["parameters"].as_array().unwrap();
        assert!(params
            .iter()
            .any(|p| p["name"] == "page" && p["schema"]["type"] == "integer"));
        assert!(params
            .iter()
            .any(|p| p["name"] == "q" && p["schema"]["type"] == "string"));

        let post = &operations["post"];
        let schema = &post["requestBody"]["content"]["application/json"]["schema"];
        assert_eq!(schema["required"], json!(["customer", "email", "amount"]));
        assert_eq!(schema["properties"]["email"]["format"], "email");
        assert_eq!(schema["properties"]["amount"]["type"], "number");
        assert_eq!(schema["properties"]["amount"]["minimum"], 0.01);
        assert_eq!(schema["properties"]["customer"]["minLength"], 2);
        assert_eq!(
            schema["properties"]["status"]["enum"],
            json!(["new", "paid"])
        );
        // Кома всередині правила — не роздільник полів.
        assert_eq!(schema["properties"]["age"]["type"], "integer");
        assert_eq!(schema["properties"]["age"]["maximum"], 120.0);
        for code in ["201", "422", "401"] {
            assert!(
                post["responses"].get(code).is_some(),
                "{code}: {}",
                post["responses"]
            );
        }
        // Коди кожної гілки — лише в її методі.
        assert!(
            get["responses"].get("201").is_none(),
            "{}",
            get["responses"]
        );
        assert!(
            get["responses"].get("422").is_none(),
            "{}",
            get["responses"]
        );
        assert!(
            get["responses"].get("200").is_some(),
            "{}",
            get["responses"]
        );
        assert!(
            get["responses"].get("401").is_some(),
            "з middleware — у всіх"
        );
        assert!(
            post["parameters"].is_null(),
            "query лише в GET: {}",
            post["parameters"]
        );
    }

    #[test]
    fn summary_is_the_first_sentence_not_the_first_line() {
        let (summary, _) = doc_comment(
            "// ЗАДАЧА: колекція — `GET` віддає список,\n// `POST` створює запис. Решта деталей.\n\nlet x = 1;",
        );
        assert_eq!(
            summary.unwrap(),
            "колекція — `GET` віддає список, `POST` створює запис."
        );
    }

    #[test]
    fn a_method_guard_limits_the_file_to_that_method() {
        let script =
            r#"if req.method != "POST" { res.status(405); return #{ error: "лише POST" }; }"#;
        let methods: Vec<String> = sections(script).into_iter().map(|(m, _)| m).collect();
        assert_eq!(methods, vec!["post"]);
    }

    #[test]
    fn middleware_codes_come_only_from_its_api_block() {
        let middleware = r#"
if req.path.starts_with("/api/") {
    if bad { res.status(401); return #{ error: "токен" }; }
    return;
}
if denied { res.status(403); }
"#;
        assert_eq!(status_codes(api_block(middleware)), BTreeSet::from([401]));
    }

    #[test]
    fn path_parameters_and_catch_all_segments() {
        assert_eq!(openapi_path("/api/files/{*rest}"), "/api/files/{rest}");
        let params = parameters("/api/orders/{id}", "");
        assert_eq!(params[0]["name"], "id");
        assert_eq!(params[0]["in"], "path");
        assert_eq!(params[0]["required"], true);
    }
}
