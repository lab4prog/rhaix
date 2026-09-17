//! Об'єкти, доступні у frontmatter: `req`, `res`, `hx`, `log`.
//!
//! Крейт навмисно нічого не знає про axum: сервер наповнює [`RequestData`]
//! звичайними рядками, а після виконання скрипта читає [`ResponseData`] і
//! перетворює на HTTP-відповідь. Завдяки цьому логіку можна виконати й
//! перевірити без жодного сокета.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rhai::{Array, Dynamic, Engine, Map};

// ------------------------------------------------------------------- запит

/// Завантажений файл: ім'я, тип і байти. Сервер наповнює це з multipart-тіла.
#[derive(Debug, Clone)]
pub struct UploadData {
    pub filename: String,
    pub content_type: String,
    pub data: Arc<Vec<u8>>,
}

/// Дані запиту в тому вигляді, у якому їх бачить `.rhx`.
#[derive(Debug, Default, Clone)]
pub struct RequestData {
    pub method: String,
    pub path: String,
    /// Сегменти маршруту: `pages/todo/[id].rhx` → `id`.
    pub params: BTreeMap<String, String>,
    pub query: BTreeMap<String, String>,
    /// Розібране тіло форми (`application/x-www-form-urlencoded` або текстові
    /// поля з `multipart/form-data`).
    pub form: BTreeMap<String, String>,
    /// Файли з `multipart/form-data`: поле → список завантажень.
    pub files: BTreeMap<String, Vec<UploadData>>,
    pub headers: BTreeMap<String, String>,
    pub cookies: BTreeMap<String, String>,
    pub body: String,
    pub is_htmx: bool,
    /// Корінь, відносно якого `upload.save(...)` пише файли. Ставить сервер.
    pub upload_root: PathBuf,
}

/// `req` у скрипті.
#[derive(Debug, Default, Clone)]
pub struct Request(Arc<RequestData>);

impl Request {
    pub fn new(data: RequestData) -> Self {
        Self(Arc::new(data))
    }

    pub fn data(&self) -> &RequestData {
        &self.0
    }
}

fn lookup(map: &BTreeMap<String, String>, name: &str) -> Dynamic {
    match map.get(name) {
        Some(value) => Dynamic::from(value.clone()),
        // Відсутнє значення — `()`, щоб працювало звичне `?? "за замовчуванням"`.
        None => Dynamic::UNIT,
    }
}

fn as_int(map: &BTreeMap<String, String>, name: &str) -> Dynamic {
    match map.get(name).and_then(|v| v.trim().parse::<i64>().ok()) {
        Some(value) => Dynamic::from(value),
        None => Dynamic::UNIT,
    }
}

fn as_float(map: &BTreeMap<String, String>, name: &str) -> Dynamic {
    match map
        .get(name)
        .and_then(|v| v.trim().replace(',', ".").parse::<f64>().ok())
    {
        Some(value) => Dynamic::from(value),
        None => Dynamic::UNIT,
    }
}

/// Прапорець із форми: `on`, `true`, `1`, `yes` — увімкнено.
fn as_bool(map: &BTreeMap<String, String>, name: &str) -> bool {
    match map.get(name) {
        Some(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "on" | "true" | "1" | "yes" | "так"
        ),
        None => false,
    }
}

/// `upload` у скрипті — один завантажений файл.
#[derive(Debug, Clone)]
pub struct Upload {
    data: UploadData,
    root: PathBuf,
}

impl Upload {
    fn new(data: UploadData, root: PathBuf) -> Self {
        Self { data, root }
    }

    /// Куди насправді писати `save(path)`.
    ///
    /// Шлях від користувача звіряється: заборонені абсолютні шляхи й `..`, щоб
    /// `save(req.form("name"))` не вивів запис за межі проєкту. Резолвиться
    /// відносно кореня проєкту — так само, як база (`data/app.db`).
    fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        let path = path.trim();
        if path.is_empty() {
            return Err("порожній шлях для save()".to_owned());
        }
        let candidate = Path::new(path);
        if candidate.is_absolute()
            || path.starts_with('/')
            || path.starts_with('\\')
            || candidate.components().any(|c| c.as_os_str() == "..")
        {
            return Err(format!("небезпечний шлях `{path}`: без абсолютних шляхів і `..`"));
        }
        Ok(self.root.join(candidate))
    }
}


/// Зареєструвати тип `Upload` і його методи.
fn register_upload(engine: &mut Engine) {
    engine
        .register_type_with_name::<Upload>("Upload")
        .register_get("filename", |u: &mut Upload| u.data.filename.clone())
        .register_get("content_type", |u: &mut Upload| u.data.content_type.clone())
        .register_get("size", |u: &mut Upload| u.data.data.len() as i64)
        // Зручні прапорці для найчастішої перевірки — тип завантаження.
        .register_get("is_image", |u: &mut Upload| {
            u.data.content_type.starts_with("image/")
        })
        // Розширення з імені файлу, у нижньому регістрі, без крапки.
        .register_get("extension", |u: &mut Upload| {
            Path::new(&u.data.filename)
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default()
        })
        // Вміст як текст — для завантажених `.csv`/`.txt`.
        .register_fn("text", |u: &mut Upload| {
            String::from_utf8_lossy(&u.data.data).into_owned()
        })
        // `save(path)` пише файл і повертає шлях, куди зберегло; помилку кидає як
        // помилку скрипта (з позицією у файлі), а не мовчить.
        .register_fn("save", |u: &mut Upload, path: &str| -> Result<String, Box<rhai::EvalAltResult>> {
            let target = u.resolve(path).map_err(|e| -> Box<rhai::EvalAltResult> { e.into() })?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| -> Box<rhai::EvalAltResult> { format!("save(): {e}").into() })?;
            }
            std::fs::write(&target, u.data.data.as_ref())
                .map_err(|e| -> Box<rhai::EvalAltResult> { format!("save(): {e}").into() })?;
            Ok(path.to_owned())
        });
}

// ---------------------------------------------------------------- відповідь

/// Те, що скрипт сказав зробити з відповіддю.
#[derive(Debug, Clone)]
pub struct ResponseData {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub cookies: Vec<String>,
    /// `hx.trigger(...)` і `hx.toast(...)` — усе це збирається в `HX-Trigger`.
    pub triggers: Vec<(String, Dynamic)>,
    /// `res.redirect(...)` / `hx.redirect(...)`.
    pub redirect: Option<String>,
    /// Чи був `hx.refresh()`.
    pub refresh: bool,
    /// Чи просив скрипт зупинити обробку (редірект або явний статус).
    pub stop: bool,
}

impl Default for ResponseData {
    fn default() -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            cookies: Vec::new(),
            triggers: Vec::new(),
            redirect: None,
            refresh: false,
            stop: false,
        }
    }
}

/// Спільний стан відповіді: `res`, `hx` і сервер бачать той самий об'єкт.
#[derive(Debug, Default, Clone)]
pub struct Response(Arc<Mutex<ResponseData>>);

impl Response {
    pub fn new() -> Self {
        Self::default()
    }

    /// Забрати зібраний стан. Мʼютекс тут не гарячий: один запит — один потік.
    pub fn take(&self) -> ResponseData {
        self.0.lock().expect("стан відповіді не отруєний").clone()
    }

    fn with(&self, f: impl FnOnce(&mut ResponseData)) {
        let mut guard = self.0.lock().expect("стан відповіді не отруєний");
        f(&mut guard);
    }
}

/// `hx` у скрипті — той самий стан, але з іншим набором методів.
#[derive(Debug, Default, Clone)]
pub struct Hx(Response);

impl Hx {
    pub fn new(response: Response) -> Self {
        Self(response)
    }
}

/// `log` у скрипті.
#[derive(Debug, Clone)]
pub struct Log {
    /// Файл, з якого пишуть — щоб у консолі було видно, хто саме.
    pub source: String,
}

// ------------------------------------------------------------- реєстрація

/// Зареєструвати типи `req`, `res`, `hx`, `log`.
pub fn register_web(engine: &mut Engine) {
    engine
        .register_type_with_name::<Request>("Request")
        .register_get("method", |req: &mut Request| req.0.method.clone())
        .register_get("path", |req: &mut Request| req.0.path.clone())
        .register_get("body", |req: &mut Request| req.0.body.clone())
        .register_get("is_htmx", |req: &mut Request| req.0.is_htmx)
        .register_fn("param", |req: &mut Request, name: &str| {
            lookup(&req.0.params, name)
        })
        .register_fn("query", |req: &mut Request, name: &str| {
            lookup(&req.0.query, name)
        })
        .register_fn("query_int", |req: &mut Request, name: &str| {
            as_int(&req.0.query, name)
        })
        .register_fn("query_float", |req: &mut Request, name: &str| {
            as_float(&req.0.query, name)
        })
        .register_fn("query_bool", |req: &mut Request, name: &str| {
            as_bool(&req.0.query, name)
        })
        .register_fn("form", |req: &mut Request, name: &str| {
            lookup(&req.0.form, name)
        })
        .register_fn("form_int", |req: &mut Request, name: &str| {
            as_int(&req.0.form, name)
        })
        .register_fn("form_float", |req: &mut Request, name: &str| {
            as_float(&req.0.form, name)
        })
        .register_fn("form_bool", |req: &mut Request, name: &str| {
            as_bool(&req.0.form, name)
        })
        .register_fn("header", |req: &mut Request, name: &str| {
            lookup(&req.0.headers, &name.to_ascii_lowercase())
        })
        .register_fn("cookie", |req: &mut Request, name: &str| {
            lookup(&req.0.cookies, name)
        })
        .register_fn("all_query", |req: &mut Request| to_map(&req.0.query))
        .register_fn("all_form", |req: &mut Request| to_map(&req.0.form))
        // `req.file(name)` — перший завантажений файл поля, або `()`.
        .register_fn("file", |req: &mut Request, name: &str| {
            match req.0.files.get(name).and_then(|list| list.first()) {
                Some(data) => Dynamic::from(Upload::new(data.clone(), req.0.upload_root.clone())),
                None => Dynamic::UNIT,
            }
        })
        // `req.files(name)` — усі файли поля (для `<input multiple>`).
        .register_fn("files", |req: &mut Request, name: &str| {
            let root = req.0.upload_root.clone();
            let array: Array = req
                .0
                .files
                .get(name)
                .map(|list| {
                    list.iter()
                        .map(|data| Dynamic::from(Upload::new(data.clone(), root.clone())))
                        .collect()
                })
                .unwrap_or_default();
            Dynamic::from_array(array)
        })
        .register_fn("has_file", |req: &mut Request, name: &str| {
            req.0.files.get(name).is_some_and(|list| !list.is_empty())
        });

    register_upload(engine);

    engine
        .register_type_with_name::<Response>("Response")
        .register_fn("status", |res: &mut Response, code: i64| {
            // Статус не скасовує рендер: сторінка з помилкою валідації віддає
            // 422 і ту саму форму з підсвіченими полями.
            res.with(|data| data.status = code.clamp(100, 599) as u16);
        })
        .register_fn("header", |res: &mut Response, name: &str, value: &str| {
            res.with(|data| data.headers.push((name.to_owned(), value.to_owned())));
        })
        .register_fn("cookie", |res: &mut Response, name: &str, value: &str| {
            res.with(|data| {
                data.cookies
                    .push(format!("{name}={value}; Path=/; HttpOnly; SameSite=Lax"));
            });
        })
        .register_fn("redirect", |res: &mut Response, url: &str| {
            res.with(|data| {
                data.redirect = Some(url.to_owned());
                data.stop = true;
            });
        });

    engine
        .register_type_with_name::<Hx>("Hx")
        .register_fn("trigger", |hx: &mut Hx, name: &str| {
            hx.0.with(|data| data.triggers.push((name.to_owned(), Dynamic::UNIT)));
        })
        .register_fn("trigger", |hx: &mut Hx, name: &str, detail: Dynamic| {
            hx.0.with(|data| data.triggers.push((name.to_owned(), detail)));
        })
        .register_fn("toast", |hx: &mut Hx, message: &str| {
            push_toast(hx, message, "info");
        })
        .register_fn("toast", |hx: &mut Hx, message: &str, kind: &str| {
            push_toast(hx, message, kind);
        })
        .register_fn("redirect", |hx: &mut Hx, url: &str| {
            hx.0.with(|data| {
                data.redirect = Some(url.to_owned());
                data.stop = true;
            });
        })
        .register_fn("refresh", |hx: &mut Hx| {
            hx.0.with(|data| {
                data.refresh = true;
                data.stop = true;
            });
        })
        .register_fn("push_url", |hx: &mut Hx, url: &str| {
            header(hx, "HX-Push-Url", url);
        })
        .register_fn("retarget", |hx: &mut Hx, selector: &str| {
            header(hx, "HX-Retarget", selector);
        })
        .register_fn("reswap", |hx: &mut Hx, mode: &str| {
            header(hx, "HX-Reswap", mode);
        })
        .register_fn("location", |hx: &mut Hx, url: &str| {
            header(hx, "HX-Location", url);
        });

    register_state(engine);

    engine
        .register_type_with_name::<Log>("Log")
        .register_fn("info", |log: &mut Log, message: Dynamic| {
            tracing::info!("{}: {}", log.source, super::display(&message));
        })
        .register_fn("warn", |log: &mut Log, message: Dynamic| {
            tracing::warn!("{}: {}", log.source, super::display(&message));
        })
        .register_fn("error", |log: &mut Log, message: Dynamic| {
            tracing::error!("{}: {}", log.source, super::display(&message));
        });
}

fn push_toast(hx: &mut Hx, message: &str, kind: &str) {
    let mut detail = Map::new();
    detail.insert("message".into(), Dynamic::from(message.to_owned()));
    detail.insert("type".into(), Dynamic::from(kind.to_owned()));
    hx.0.with(|data| {
        data.triggers
            .push(("showToast".to_owned(), Dynamic::from_map(detail)))
    });
}

fn header(hx: &mut Hx, name: &str, value: &str) {
    hx.0.with(|data| data.headers.push((name.to_owned(), value.to_owned())));
}

fn to_map(values: &BTreeMap<String, String>) -> Map {
    let mut map = Map::new();
    for (key, value) in values {
        map.insert(key.as_str().into(), Dynamic::from(value.clone()));
    }
    map
}

/// `state` — процесне сховище ключ-значення.
///
/// Аналог `global.set` у Node-RED. Формально це частина M5, але без нього
/// демо-форма не має де тримати дані, тому зроблено раніше. Дані живуть до
/// перезапуску процесу і не переживають рестарт — для чогось серйознішого
/// буде `db`.
#[derive(Debug, Default, Clone)]
pub struct State(Arc<Mutex<Map>>);

impl State {
    pub fn new() -> Self {
        Self::default()
    }
}

fn register_state(engine: &mut Engine) {
    engine
        .register_type_with_name::<State>("State")
        .register_fn("get", |state: &mut State, key: &str| {
            state
                .0
                .lock()
                .expect("сховище не отруєне")
                .get(key)
                .cloned()
                .unwrap_or(Dynamic::UNIT)
        })
        .register_fn("set", |state: &mut State, key: &str, value: Dynamic| {
            state
                .0
                .lock()
                .expect("сховище не отруєне")
                .insert(key.into(), value);
        })
        .register_fn("has", |state: &mut State, key: &str| {
            state
                .0
                .lock()
                .expect("сховище не отруєне")
                .contains_key(key)
        })
        .register_fn("remove", |state: &mut State, key: &str| {
            state.0.lock().expect("сховище не отруєне").remove(key);
        });
}

/// Зібрати заголовок `HX-Trigger` з подій, які накидав скрипт.
///
/// `hx.trigger("x")` без даних дає `true`, `hx.toast(...)` — обʼєкт із
/// повідомленням; саме це чекає `rhaix.js` на клієнті.
pub fn triggers_header(triggers: &[(String, Dynamic)]) -> Option<String> {
    if triggers.is_empty() {
        return None;
    }
    let mut object = serde_json::Map::new();
    for (name, detail) in triggers {
        let value = if detail.is_unit() {
            serde_json::Value::Bool(true)
        } else {
            serde_json::to_value(detail).unwrap_or(serde_json::Value::Bool(true))
        };
        object.insert(name.clone(), value);
    }
    Some(escape_non_ascii(
        &serde_json::Value::Object(object).to_string(),
    ))
}

/// Значення HTTP-заголовка має бути ASCII, інакше воно просто не пройде:
/// `HeaderValue::from_str` його відхилить, і тост зникне без сліду.
///
/// JSON це дозволяє: будь-який символ записується як `\uXXXX`, а `JSON.parse`
/// на клієнті повертає його назад. Знайдено на тості з кирилицею в M2.
fn escape_non_ascii(text: &str) -> String {
    if text.is_ascii() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len() * 2);
    for ch in text.chars() {
        if ch.is_ascii() {
            out.push(ch);
            continue;
        }
        let mut buffer = [0u16; 2];
        for unit in ch.encode_utf16(&mut buffer) {
            out.push_str(&format!("\\u{unit:04x}"));
        }
    }
    out
}

// ------------------------------------------------------- розбір параметрів

/// Розібрати `a=1&b=%D0%B0` у мапу. Власний розбір замість залежності:
/// формат простий, а зайвий крейт тут нічого не додає.
pub fn parse_urlencoded(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for pair in text.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = match pair.split_once('=') {
            Some((key, value)) => (key, value),
            None => (pair, ""),
        };
        out.insert(decode_component(key), decode_component(value));
    }
    out
}

/// Розібрати заголовок `Cookie`.
pub fn parse_cookies(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for pair in text.split(';') {
        if let Some((key, value)) = pair.split_once('=') {
            out.insert(key.trim().to_owned(), value.trim().to_owned());
        }
    }
    out
}

fn decode_component(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                match u8::from_str_radix(&text[index + 1..index + 3], 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        out.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            other => {
                out.push(other);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_save_path_rejects_traversal() {
        let up = Upload::new(
            UploadData {
                filename: "x.png".into(),
                content_type: "image/png".into(),
                data: Arc::new(vec![1, 2, 3]),
            },
            PathBuf::from("/proj"),
        );
        // Нормальний шлях резолвиться під коренем.
        assert!(up.resolve("public/uploads/x.png").is_ok());
        // Абсолютний і `..` — відмова.
        assert!(up.resolve("/etc/passwd").is_err());
        assert!(up.resolve("../../secret").is_err());
        assert!(up.resolve("public/../../x").is_err());
        assert!(up.resolve("").is_err());
    }
    use crate::{engine, Limits};
    use rhai::Scope;

    fn run(script: &str, request: RequestData) -> (Dynamic, ResponseData) {
        let engine = engine(Limits::default());
        let response = Response::new();
        let mut scope = Scope::new();
        scope.push("req", Request::new(request));
        scope.push("res", response.clone());
        scope.push("hx", Hx::new(response.clone()));
        let value = engine
            .eval_with_scope::<Dynamic>(&mut scope, script)
            .expect("скрипт має виконатись");
        (value, response.take())
    }

    fn request() -> RequestData {
        RequestData {
            method: "POST".into(),
            path: "/orders".into(),
            query: parse_urlencoded("page=2&q=%D0%BC%D0%BE%D0%BB%D0%BE%D0%BA%D0%BE&empty="),
            form: parse_urlencoded("title=Hello+world&qty=3&urgent=on"),
            cookies: parse_cookies("sid=abc; theme=dark"),
            is_htmx: true,
            ..Default::default()
        }
    }

    #[test]
    fn request_exposes_typed_accessors() {
        let (value, _) = run(
            r#"[req.method, req.query("q"), req.query_int("page"), req.form("title"),
               req.form_int("qty"), req.form_bool("urgent"), req.cookie("theme"), req.is_htmx]"#,
            request(),
        );
        let array = value.cast::<rhai::Array>();
        assert_eq!(array[0].clone().cast::<String>(), "POST");
        assert_eq!(array[1].clone().cast::<String>(), "молоко");
        assert_eq!(array[2].clone().cast::<i64>(), 2);
        assert_eq!(array[3].clone().cast::<String>(), "Hello world");
        assert_eq!(array[4].clone().cast::<i64>(), 3);
        assert!(array[5].clone().cast::<bool>());
        assert_eq!(array[6].clone().cast::<String>(), "dark");
        assert!(array[7].clone().cast::<bool>());
    }

    #[test]
    fn missing_values_are_unit_so_defaults_work() {
        let (value, _) = run(
            r#"[req.query("nope") ?? "за замовчуванням", req.query_int("q") ?? 7]"#,
            request(),
        );
        let array = value.cast::<rhai::Array>();
        assert_eq!(array[0].clone().cast::<String>(), "за замовчуванням");
        assert_eq!(array[1].clone().cast::<i64>(), 7);
    }

    #[test]
    fn hx_collects_triggers_and_toasts() {
        let (_, response) = run(
            r#"hx.toast("Додано", "success"); hx.trigger("todoChanged"); hx.push_url("/todo")"#,
            request(),
        );
        assert_eq!(response.triggers.len(), 2);
        assert_eq!(response.triggers[0].0, "showToast");
        assert_eq!(response.triggers[1].0, "todoChanged");
        assert_eq!(response.headers[0], ("HX-Push-Url".into(), "/todo".into()));
    }

    #[test]
    fn redirect_stops_rendering() {
        let (_, response) = run(r#"res.redirect("/login")"#, request());
        assert_eq!(response.redirect.as_deref(), Some("/login"));
        assert!(response.stop);
    }

    #[test]
    fn status_alone_does_not_cancel_rendering() {
        // 422 + перерендерена форма з помилками — звичайний сценарій,
        // тому статус сам по собі нічого не зупиняє.
        let (_, response) = run("res.status(422)", request());
        assert_eq!(response.status, 422);
        assert!(!response.stop);
    }

    #[test]
    fn trigger_header_is_valid_json() {
        let (_, response) = run(
            r#"hx.toast("Готово", "success"); hx.trigger("todoChanged")"#,
            request(),
        );
        let header = triggers_header(&response.triggers).expect("є події");
        assert!(header.contains(r#""showToast":{"#), "{header}");
        assert!(header.contains(r#""todoChanged":true"#), "{header}");
        // кирилиця — через \uXXXX, інакше заголовок не пройде у відповідь
        assert!(header.is_ascii(), "{header}");
        assert!(header.contains(r"\u0413"), "{header}");
    }

    #[test]
    fn urlencoded_decoding_handles_plus_and_utf8() {
        let map = parse_urlencoded("a=1+2&b=%D1%82%D0%B5%D1%81%D1%82&c");
        assert_eq!(map["a"], "1 2");
        assert_eq!(map["b"], "тест");
        assert_eq!(map["c"], "");
    }
}
