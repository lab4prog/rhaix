//! `validate(значення, правила)` — перевірка форми одним викликом.
//!
//! Замість десятка `if is_blank(...) { errors.x = "..." }` у кожному файлі —
//! декларативні правила у стилі, знайомому з Laravel/Symfony:
//!
//! ```rhai
//! let errors = validate(req.all_form(), #{
//!     name:  "required",
//!     email: "required|email",
//!     age:   "int|between:18,120",
//!     site:  "url",
//!     pass:  "required|min:8",
//!     again: "same:pass",
//! });
//! if errors.is_empty() { /* зберегти */ } else { res.status(422); }
//! ```
//!
//! Повертає мапу `поле → повідомлення` (порожню, якщо все гаразд). Повідомлення
//! українською — їх видно користувачеві поруч із полем.

use rhai::{Dynamic, Engine, Map};

/// Одне правило, вже розібране на ім'я й аргументи.
struct Rule<'a> {
    name: &'a str,
    args: Vec<&'a str>,
}

fn parse_rules(spec: &str) -> Vec<Rule<'_>> {
    spec.split('|')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (name, rest) = match part.split_once(':') {
                Some((name, rest)) => (name.trim(), rest),
                None => (part, ""),
            };
            let args = if rest.is_empty() {
                Vec::new()
            } else {
                rest.split(',').map(str::trim).collect()
            };
            Rule { name, args }
        })
        .collect()
}

/// Значення поля як рядок (те, що приходить із форми).
fn field_value(values: &Map, field: &str) -> String {
    values.get(field).map(super::display).unwrap_or_default()
}

/// Чи має поле числове правило — тоді `min`/`max`/`between` порівнюють значення,
/// а не довжину рядка.
fn is_numeric_field(rules: &[Rule]) -> bool {
    rules.iter().any(|r| matches!(r.name, "int" | "number"))
}

/// Перевірити одне поле за списком правил. Повертає перше повідомлення, що не
/// пройшло, або `None`.
fn check_field(values: &Map, field: &str, spec: &str) -> Option<String> {
    let rules = parse_rules(spec);
    let value = field_value(values, field);
    let trimmed = value.trim();
    let numeric = is_numeric_field(&rules);

    // Порожнє поле без `required` — не помилка: решта правил до нього не
    // застосовується (порожній необов'язковий email валідний).
    let required = rules.iter().any(|r| r.name == "required");
    if trimmed.is_empty() && !required {
        return None;
    }

    for rule in &rules {
        let ok = match rule.name {
            "required" => !trimmed.is_empty(),
            "email" => is_email(trimmed),
            "url" => is_url(trimmed),
            "int" => trimmed.parse::<i64>().is_ok(),
            "number" => trimmed.replace(',', ".").parse::<f64>().is_ok(),
            "min" => compare(trimmed, rule.args.first(), numeric, Cmp::Min),
            "max" => compare(trimmed, rule.args.first(), numeric, Cmp::Max),
            "between" => {
                compare(trimmed, rule.args.first(), numeric, Cmp::Min)
                    && compare(trimmed, rule.args.get(1), numeric, Cmp::Max)
            }
            "same" => rule
                .args
                .first()
                .map(|other| value == field_value(values, other))
                .unwrap_or(true),
            "different" => rule
                .args
                .first()
                .map(|other| value != field_value(values, other))
                .unwrap_or(true),
            "in" => rule.args.contains(&trimmed),
            "not_in" => !rule.args.contains(&trimmed),
            // Дата в тому ж вигляді, у якому її віддає база й приймає `date()`.
            "date" => crate::datetime::parse(trimmed).is_some(),
            "bool" => matches!(
                trimmed.to_ascii_lowercase().as_str(),
                "true" | "false" | "1" | "0" | "on" | "off" | "yes" | "no" | "так" | "ні"
            ),
            // `alpha`/`alnum` — за Unicode, а не ASCII: «Оля» має проходити.
            "alpha" => trimmed
                .chars()
                .all(|c| c.is_alphabetic() || c == ' ' || c == '-' || c == '\''),
            "alnum" => trimmed
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-'),
            "len" => rule
                .args
                .first()
                .and_then(|n| n.parse::<usize>().ok())
                .map(|n| trimmed.chars().count() == n)
                .unwrap_or(false),
            "starts" => rule
                .args
                .first()
                .map(|p| trimmed.starts_with(p))
                .unwrap_or(true),
            "ends" => rule
                .args
                .first()
                .map(|p| trimmed.ends_with(p))
                .unwrap_or(true),
            // Невідоме правило не має мовчки пропускати поле: це помилка автора.
            other => return Some(format!("невідоме правило `{other}`")),
        };
        if !ok {
            return Some(message(rule, numeric));
        }
    }
    None
}

enum Cmp {
    Min,
    Max,
}

/// Порівняти значення з межею: для числового поля — за величиною, інакше — за
/// довжиною рядка.
fn compare(value: &str, bound: Option<&&str>, numeric: bool, cmp: Cmp) -> bool {
    let Some(bound) = bound else { return true };
    if numeric {
        let (Ok(v), Ok(b)) = (value.replace(',', ".").parse::<f64>(), bound.parse::<f64>()) else {
            return false;
        };
        match cmp {
            Cmp::Min => v >= b,
            Cmp::Max => v <= b,
        }
    } else {
        let Ok(b) = bound.parse::<usize>() else {
            return false;
        };
        let len = value.chars().count();
        match cmp {
            Cmp::Min => len >= b,
            Cmp::Max => len <= b,
        }
    }
}

fn message(rule: &Rule, numeric: bool) -> String {
    let arg = |i: usize| rule.args.get(i).copied().unwrap_or("");
    match rule.name {
        "required" => "Обов'язкове поле".to_owned(),
        "email" => "Схоже, це не пошта".to_owned(),
        "url" => "Схоже, це не адреса".to_owned(),
        "int" => "Має бути цілим числом".to_owned(),
        "number" => "Має бути числом".to_owned(),
        "min" if numeric => format!("Не менше за {}", arg(0)),
        "min" => format!("Щонайменше {} символів", arg(0)),
        "max" if numeric => format!("Не більше за {}", arg(0)),
        "max" => format!("Не більше за {} символів", arg(0)),
        "between" if numeric => format!("Від {} до {}", arg(0), arg(1)),
        "between" => format!("Від {} до {} символів", arg(0), arg(1)),
        "same" => "Значення не збігаються".to_owned(),
        "different" => "Значення має відрізнятися".to_owned(),
        "in" | "not_in" => "Неприпустиме значення".to_owned(),
        "date" => "Схоже, це не дата".to_owned(),
        "bool" => "Має бути так або ні".to_owned(),
        "alpha" => "Лише літери".to_owned(),
        "alnum" => "Лише літери й цифри".to_owned(),
        "len" => format!("Рівно {} символів", arg(0)),
        "starts" => format!("Має починатися з `{}`", arg(0)),
        "ends" => format!("Має закінчуватися на `{}`", arg(0)),
        other => format!("невідоме правило `{other}`"),
    }
}

fn is_email(text: &str) -> bool {
    // Не RFC 5322 — навмисно: повна перевірка пошти неможлива без надсилання
    // листа. Ловимо очевидне: одна @, щось до неї, домен із крапкою після.
    let Some((local, domain)) = text.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.len() >= 3
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !text.contains(' ')
}

fn is_url(text: &str) -> bool {
    (text.starts_with("http://") || text.starts_with("https://"))
        && text.len() > 8
        && !text.contains(' ')
}

/// `validate(значення, правила)` → мапа помилок.
pub fn validate(values: &Map, rules: &Map) -> Map {
    let mut errors = Map::new();
    for (field, spec) in rules.iter() {
        let spec = super::display(spec);
        if let Some(problem) = check_field(values, field.as_str(), &spec) {
            errors.insert(field.clone(), Dynamic::from(problem));
        }
    }
    errors
}

pub fn register_validate(engine: &mut Engine) {
    engine.register_fn("validate", |values: Map, rules: Map| {
        Dynamic::from_map(validate(&values, &rules))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(pairs: &[(&str, &str)]) -> Map {
        pairs
            .iter()
            .map(|(k, v)| ((*k).into(), Dynamic::from((*v).to_owned())))
            .collect()
    }

    fn errors(values: &[(&str, &str)], rules: &[(&str, &str)]) -> Map {
        let r: Map = rules
            .iter()
            .map(|(k, v)| ((*k).into(), Dynamic::from((*v).to_owned())))
            .collect();
        validate(&form(values), &r)
    }

    #[test]
    fn required_and_email() {
        let e = errors(
            &[("name", ""), ("email", "не пошта")],
            &[("name", "required"), ("email", "required|email")],
        );
        assert!(e.contains_key("name"));
        assert!(e.contains_key("email"));

        let ok = errors(
            &[("name", "Оля"), ("email", "olya@example.com")],
            &[("name", "required"), ("email", "required|email")],
        );
        assert!(ok.is_empty(), "{ok:?}");
    }

    #[test]
    fn empty_optional_field_skips_other_rules() {
        // Порожній необов'язковий email не має давати помилку email.
        let e = errors(&[("site", "")], &[("site", "url")]);
        assert!(e.is_empty(), "{e:?}");
    }

    #[test]
    fn numeric_min_max_compare_by_value() {
        let e = errors(&[("age", "5")], &[("age", "int|between:18,120")]);
        assert_eq!(e["age"].clone().cast::<String>(), "Від 18 до 120");

        let ok = errors(&[("age", "30")], &[("age", "int|between:18,120")]);
        assert!(ok.is_empty());
    }

    #[test]
    fn string_min_max_compare_by_length() {
        let e = errors(&[("pass", "abc")], &[("pass", "min:8")]);
        assert_eq!(e["pass"].clone().cast::<String>(), "Щонайменше 8 символів");

        let ok = errors(&[("pass", "abcdefgh")], &[("pass", "min:8")]);
        assert!(ok.is_empty());
    }

    #[test]
    fn same_checks_confirmation() {
        let e = errors(
            &[("pass", "секрет"), ("again", "інше")],
            &[("again", "same:pass")],
        );
        assert!(e.contains_key("again"));

        let ok = errors(
            &[("pass", "секрет"), ("again", "секрет")],
            &[("again", "same:pass")],
        );
        assert!(ok.is_empty());
    }

    #[test]
    fn in_restricts_to_a_set() {
        let e = errors(&[("role", "root")], &[("role", "in:user,admin")]);
        assert!(e.contains_key("role"));
        let ok = errors(&[("role", "admin")], &[("role", "in:user,admin")]);
        assert!(ok.is_empty());
    }

    #[test]
    fn email_edge_cases() {
        assert!(is_email("a@b.co"));
        assert!(!is_email("a@b"));
        assert!(!is_email("@b.co"));
        assert!(!is_email("a b@c.co"));
        assert!(!is_email("a@.co"));
    }

    #[test]
    fn extra_rules_cover_common_cases() {
        // date — той самий формат, що віддає база.
        assert!(errors(&[("d", "2026-09-17")], &[("d", "date")]).is_empty());
        assert!(errors(&[("d", "позавчора")], &[("d", "date")]).contains_key("d"));

        // alpha за Unicode: кирилиця проходить, цифри — ні.
        assert!(errors(&[("n", "Оля Литвин")], &[("n", "alpha")]).is_empty());
        assert!(errors(&[("n", "Оля2")], &[("n", "alpha")]).contains_key("n"));

        // len — рівно стільки символів (рахуються символи, не байти).
        assert!(errors(&[("c", "UAH")], &[("c", "len:3")]).is_empty());
        assert!(errors(&[("c", "грн")], &[("c", "len:3")]).is_empty());
        assert!(errors(&[("c", "UA")], &[("c", "len:3")]).contains_key("c"));

        // different — протилежність same.
        assert!(errors(&[("a", "x"), ("b", "y")], &[("b", "different:a")]).is_empty());
        assert!(errors(&[("a", "x"), ("b", "x")], &[("b", "different:a")]).contains_key("b"));

        // not_in, starts, ends, bool.
        assert!(errors(&[("r", "root")], &[("r", "not_in:root,admin")]).contains_key("r"));
        assert!(errors(&[("u", "https://a.co")], &[("u", "starts:https://")]).is_empty());
        assert!(errors(&[("f", "a.png")], &[("f", "ends:.png")]).is_empty());
        assert!(errors(&[("b", "так")], &[("b", "bool")]).is_empty());
        assert!(errors(&[("b", "можливо")], &[("b", "bool")]).contains_key("b"));
    }

    #[test]
    fn an_unknown_rule_is_reported() {
        let e = errors(&[("x", "1")], &[("x", "wat")]);
        assert!(e["x"].clone().cast::<String>().contains("невідоме правило"));
    }
}
