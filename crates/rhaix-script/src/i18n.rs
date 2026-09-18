//! `t("ключ")` — переклади.
//!
//! Файли лежать у `locales/<мова>.toml` і читаються через той самий трейт
//! `Files`, що й шаблони, тож переклади працюють і у вшитому бінарнику.
//!
//! ```toml
//! # locales/uk.toml
//! greeting = "Привіт"
//! items    = "У кошику {count} товарів"
//! ```
//!
//! ```rhai
//! t("greeting")                       // "Привіт"
//! t("items", #{ count: 3 })           // "У кошику 3 товарів"
//! set_locale("en");                   // зазвичай у middleware.rhx
//! locale()                            // поточна мова
//! ```
//!
//! **Відсутній ключ повертає сам ключ**, а не порожній рядок: дірку в перекладі
//! видно на сторінці одразу, і вона не виглядає як загублений текст.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use rhai::{Engine, Map};

/// Усі переклади: мова → (ключ → текст). Будується один раз при старті.
pub type Catalog = BTreeMap<String, BTreeMap<String, String>>;

/// `t`/`locale` у скрипті. Мова — на запит, каталог — спільний.
#[derive(Clone)]
pub struct I18n {
    catalog: Arc<Catalog>,
    /// Мова за замовчуванням із `[app] locale`.
    fallback: Arc<String>,
    /// Поточна мова запиту; `set_locale` змінює саме її.
    current: Arc<Mutex<String>>,
}

impl std::fmt::Debug for I18n {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("I18n")
    }
}

impl I18n {
    pub fn new(catalog: Arc<Catalog>, fallback: String) -> Self {
        Self {
            current: Arc::new(Mutex::new(fallback.clone())),
            fallback: Arc::new(fallback),
            catalog,
        }
    }

    pub fn locale(&self) -> String {
        self.current.lock().expect("мова не отруєна").clone()
    }

    /// Змінити мову запиту. Невідома мова ігнорується з попередженням: інакше
    /// `?lang=xx` із адреси мовчки перетворював би всі підписи на ключі.
    pub fn set_locale(&self, locale: &str) {
        if self.catalog.contains_key(locale) {
            *self.current.lock().expect("мова не отруєна") = locale.to_owned();
        } else if !self.catalog.is_empty() {
            tracing::warn!("невідома мова `{locale}`; лишаємось на `{}`", self.locale());
        }
    }

    /// Знайти рядок: поточна мова → мова за замовчуванням → сам ключ.
    fn lookup(&self, key: &str) -> String {
        let current = self.locale();
        self.catalog
            .get(&current)
            .and_then(|m| m.get(key))
            .or_else(|| self.catalog.get(self.fallback.as_str()).and_then(|m| m.get(key)))
            .cloned()
            .unwrap_or_else(|| key.to_owned())
    }

    fn translate(&self, key: &str) -> String {
        self.lookup(key)
    }

    fn translate_with(&self, key: &str, params: &Map) -> String {
        interpolate(&self.lookup(key), params)
    }
}

/// Підставити `{ім'я}` значеннями з мапи.
///
/// Навмисно без екранування: результат `t(...)` — звичайний рядок, і в шаблоні
/// він проходить те саме екранування, що й будь-який інший текст.
fn interpolate(template: &str, params: &Map) -> String {
    if !template.contains('{') {
        return template.to_owned();
    }
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) => {
                let name = &after[..end];
                match params.get(name) {
                    Some(value) => out.push_str(&super::display(value)),
                    // Немає значення — лишаємо плейсхолдер видимим: так одразу
                    // зрозуміло, якого параметра бракує.
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Розібрати `locales/*.toml` найпростішим способом: `ключ = "значення"`.
///
/// Свій розбір замість залежності на `toml` тут свідомий: формат словника — це
/// плоскі пари, а крейт із повним TOML у `rhaix-script` тягнути нема за чим.
pub fn parse_catalog_file(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        // Значення в лапках; усе інше лишаємо як є.
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value);
        if !key.is_empty() {
            out.insert(key.to_owned(), unescape(value));
        }
    }
    out
}

/// `\n` і `\"` у значенні TOML.
fn unescape(text: &str) -> String {
    if !text.contains('\\') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

thread_local! {
    /// Переклади поточного запиту.
    ///
    /// Так само, як `Deadline`: рендер іде в `spawn_blocking`, тобто один запит —
    /// один потік. Завдяки цьому `t("ключ")` лишається **вільною функцією**, а не
    /// методом на об'єкті: у шаблоні це читається краще, ніж `i18n.t(...)`.
    static CURRENT: RefCell<Option<I18n>> = const { RefCell::new(None) };
}

/// Прив'язати переклади до цього потоку на час запиту.
#[must_use = "переклади діють, поки живе цей об'єкт"]
pub struct LocaleScope;

impl LocaleScope {
    pub fn new(i18n: I18n) -> Self {
        CURRENT.with(|cell| *cell.borrow_mut() = Some(i18n));
        Self
    }
}

impl Drop for LocaleScope {
    fn drop(&mut self) {
        CURRENT.with(|cell| *cell.borrow_mut() = None);
    }
}

/// Виконати щось із перекладами запиту; без них — розумний відкат.
fn with_current<T>(f: impl FnOnce(&I18n) -> T, fallback: T) -> T {
    CURRENT.with(|cell| match cell.borrow().as_ref() {
        Some(i18n) => f(i18n),
        None => fallback,
    })
}

pub fn register_i18n(engine: &mut Engine) {
    engine
        .register_type_with_name::<I18n>("I18n")
        // Без перекладів ключ повертається собою — так само, як відсутній ключ.
        .register_fn("t", |key: &str| {
            with_current(|i| i.translate(key), key.to_owned())
        })
        .register_fn("t", |key: &str, params: Map| {
            with_current(|i| i.translate_with(key, &params), key.to_owned())
        })
        .register_fn("locale", || with_current(|i| i.locale(), String::new()))
        .register_fn("set_locale", |locale: &str| {
            with_current(|i| i.set_locale(locale), ());
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhai::Dynamic;

    fn catalog() -> Arc<Catalog> {
        let mut uk = BTreeMap::new();
        uk.insert("greeting".to_owned(), "Привіт".to_owned());
        uk.insert("items".to_owned(), "У кошику {count} товарів".to_owned());
        let mut en = BTreeMap::new();
        en.insert("greeting".to_owned(), "Hello".to_owned());

        let mut c = Catalog::new();
        c.insert("uk".to_owned(), uk);
        c.insert("en".to_owned(), en);
        Arc::new(c)
    }

    fn i18n() -> I18n {
        I18n::new(catalog(), "uk".to_owned())
    }

    #[test]
    fn translates_and_switches_locale() {
        let t = i18n();
        assert_eq!(t.translate("greeting"), "Привіт");
        t.set_locale("en");
        assert_eq!(t.locale(), "en");
        assert_eq!(t.translate("greeting"), "Hello");
    }

    #[test]
    fn falls_back_to_the_default_locale() {
        // `items` є лише в uk — англійська сторінка не має лишитись без тексту.
        let t = i18n();
        t.set_locale("en");
        assert_eq!(t.translate("items"), "У кошику {count} товарів");
    }

    #[test]
    fn a_missing_key_shows_itself() {
        assert_eq!(i18n().translate("nope.missing"), "nope.missing");
    }

    #[test]
    fn unknown_locale_is_ignored() {
        let t = i18n();
        t.set_locale("xx");
        assert_eq!(t.locale(), "uk", "невідома мова не має скидати переклад");
    }

    #[test]
    fn parameters_are_interpolated() {
        let t = i18n();
        let params: Map = [("count".into(), Dynamic::from(3_i64))].into_iter().collect();
        assert_eq!(t.translate_with("items", &params), "У кошику 3 товарів");

        // Бракує параметра — плейсхолдер лишається видимим.
        assert_eq!(
            t.translate_with("items", &Map::new()),
            "У кошику {count} товарів"
        );
    }

    #[test]
    fn free_functions_use_the_request_scope() {
        // `t(...)` — вільна функція; вона бачить переклади цього потоку.
        let engine = crate::engine(crate::Limits::default());
        let _scope = LocaleScope::new(i18n());

        let value: String = engine.eval(r#"t("greeting")"#).expect("виклик");
        assert_eq!(value, "Привіт");

        let value: String = engine
            .eval(r#"set_locale("en"); t("greeting")"#)
            .expect("виклик");
        assert_eq!(value, "Hello");
    }

    #[test]
    fn without_a_scope_the_key_comes_back() {
        // Поза запитом (наприклад у тесті шаблону) `t` не має падати.
        let engine = crate::engine(crate::Limits::default());
        let value: String = engine.eval(r#"t("greeting")"#).expect("виклик");
        assert_eq!(value, "greeting");
    }

    #[test]
    fn catalog_file_is_parsed() {
        let map = parse_catalog_file(
            "# коментар\n[section]\ngreeting = \"Привіт\"\nmulti = \"рядок\\nдругий\"\n",
        );
        assert_eq!(map["greeting"], "Привіт");
        assert_eq!(map["multi"], "рядок\nдругий");
        assert!(!map.contains_key("# коментар"));
    }
}
