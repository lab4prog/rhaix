//! Рядки, числа й ідентифікатори — те, без чого не обходиться жодна сторінка.
//!
//! Rhai має `to_upper`, `sub_string`, `split`, `replace`. Тут лише те, чого в
//! ньому немає, а в реальному шаблоні треба щоразу: URL-адреса зі слова,
//! обрізаний анонс, ціна з пробілом між тисячами.

use rhai::{Dynamic, Engine, Map};

use crate::crypto::{random_token, sha256_hex, uuid_v4};
use crate::json;

/// Транслітерація для `slug()`.
///
/// Таблиця — за постановою КМУ № 55 (той самий запис, що в закордонному
/// паспорті), спрощена: `slug` іде в адресу, а не в документ.
fn translit(ch: char) -> Option<&'static str> {
    Some(match ch {
        'а' => "a", 'б' => "b", 'в' => "v", 'г' => "h", 'ґ' => "g",
        'д' => "d", 'е' => "e", 'є' => "ie", 'ж' => "zh", 'з' => "z",
        'и' => "y", 'і' => "i", 'ї' => "i", 'й' => "i", 'к' => "k",
        'л' => "l", 'м' => "m", 'н' => "n", 'о' => "o", 'п' => "p",
        'р' => "r", 'с' => "s", 'т' => "t", 'у' => "u", 'ф' => "f",
        'х' => "kh", 'ц' => "ts", 'ч' => "ch", 'ш' => "sh", 'щ' => "shch",
        'ь' => "", 'ю' => "iu", 'я' => "ia", '\'' => "", '’' => "",
        // Російські літери, яких немає в українській абетці: текст буває різний.
        'ё' => "e", 'ъ' => "", 'ы' => "y", 'э' => "e",
        _ => return None,
    })
}

/// `slug("Привіт, світе!")` → `pryvit-svite`.
pub fn slug(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_dash = false;

    for ch in text.chars() {
        let lower = ch.to_lowercase().next().unwrap_or(ch);
        let piece: Option<String> = if lower.is_ascii_alphanumeric() {
            Some(lower.to_string())
        } else {
            translit(lower).map(|s| s.to_owned())
        };

        match piece {
            Some(piece) if piece.is_empty() => {}
            Some(piece) => {
                if pending_dash && !out.is_empty() {
                    out.push('-');
                }
                pending_dash = false;
                out.push_str(&piece);
            }
            // Будь-що інше — роздільник. Дефіс ставимо лише перед наступною
            // літерою, тому `«Привіт!!!»` не дає хвоста з дефісів.
            None => pending_dash = true,
        }
    }
    out
}

/// `money(1234.5)` → `1 234,50`.
///
/// Формат український: нерозривний пробіл між тисячами, кома як роздільник.
/// Пробіл саме нерозривний — інакше `1 234,50 грн` переноситься посеред числа.
pub fn money(value: f64, decimals: usize) -> String {
    let negative = value < 0.0;
    // `{:.2}` у Rust округлює до парного: 42.125 стає 42.12. Для ціни це
    // несподіванка, тому округлюємо самі — від нуля, як у касовому чеку.
    let factor = 10f64.powi(decimals as i32);
    let rounded = (value.abs() * factor).round() / factor;
    let text = format!("{:.*}", decimals, rounded);
    let (whole, fraction) = match text.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (text.as_str(), None),
    };

    let mut grouped = String::with_capacity(whole.len() + whole.len() / 3 + 4);
    for (index, ch) in whole.chars().enumerate() {
        if index > 0 && (whole.len() - index) % 3 == 0 {
            grouped.push('\u{00a0}');
        }
        grouped.push(ch);
    }

    let mut out = String::with_capacity(grouped.len() + decimals + 2);
    if negative {
        out.push('-');
    }
    out.push_str(&grouped);
    if let Some(fraction) = fraction {
        out.push(',');
        out.push_str(fraction);
    }
    out
}

/// `cut(text, 20)` — обрізати по межі слова й додати трикрапку.
///
/// Назва не `truncate`: у Rhai це вже метод рядка, який ріже на місці.
pub fn cut(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let head: String = text.chars().take(limit).collect();
    let trimmed = match head.rfind(char::is_whitespace) {
        // Різати посеред слова негарно, але й лишати два символи теж:
        // повертаємось до пробілу, тільки якщо втрачаємо небагато.
        Some(index) if index >= limit / 2 => &head[..index],
        _ => head.trim_end(),
    };
    format!("{}…", trimmed.trim_end_matches([' ', ',', '.', ';', ':']))
}

/// Прибрати теги — для анонса з поля, куди могли вставити HTML.
pub fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut inside = false;
    for ch in text.chars() {
        match ch {
            '<' => inside = true,
            '>' => inside = false,
            other if !inside => out.push(other),
            _ => {}
        }
    }
    out
}

/// Перша літера велика, решта без змін.
pub fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

pub fn register_text(engine: &mut Engine) {
    engine
        .register_fn("slug", slug)
        .register_fn("cut", |text: &str, limit: i64| {
            cut(text, limit.max(0) as usize)
        })
        .register_fn("strip_tags", strip_tags)
        .register_fn("capitalize", capitalize)
        .register_fn("money", |value: f64| money(value, 2))
        .register_fn("money", |value: i64| money(value as f64, 2))
        .register_fn("money", |value: f64, decimals: i64| {
            money(value, decimals.clamp(0, 10) as usize)
        })
        .register_fn("uuid", uuid_v4)
        .register_fn("random_id", || random_token(12))
        .register_fn("sha256", sha256_hex)
        // `json_encode` — це рядок для бази чи API; для `<script>` є `json()`,
        // який ще й екранує те, що може закрити тег.
        .register_fn("json_encode", |value: Dynamic| {
            json::from_dynamic(&value).to_string()
        })
        .register_fn("json_decode", |text: &str| {
            json::parse(text).unwrap_or(Dynamic::UNIT)
        })
        // Порожнє значення: `()`, `""`, порожній масив чи мапа. Найчастіша
        // перевірка у формах, і писати її щоразу вручну нудно.
        .register_fn("is_blank", |value: Dynamic| match value.type_name() {
            "()" => true,
            "string" => value.into_string().map(|s| s.trim().is_empty()).unwrap_or(true),
            "array" => value.cast::<rhai::Array>().is_empty(),
            "map" => value.cast::<Map>().is_empty(),
            _ => false,
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_transliterates_and_joins_with_dashes() {
        assert_eq!(slug("Привіт, світе!"), "pryvit-svite");
        assert_eq!(slug("Їжак і ґанок"), "izhak-i-ganok");
        assert_eq!(slug("  Hello   World  "), "hello-world");
        assert_eq!(slug("Стаття №5"), "stattia-5");
        assert_eq!(slug("!!!"), "");
        assert_eq!(slug("м'ята"), "miata");
    }

    #[test]
    fn money_groups_thousands() {
        assert_eq!(money(1234.5, 2), "1\u{a0}234,50");
        assert_eq!(money(999.0, 2), "999,00");
        assert_eq!(money(1_000_000.0, 0), "1\u{a0}000\u{a0}000");
        assert_eq!(money(-42.125, 2), "-42,13");
        assert_eq!(money(0.0, 2), "0,00");
    }

    #[test]
    fn cut_respects_word_boundaries() {
        assert_eq!(cut("коротко", 20), "коротко");
        assert_eq!(cut("один два три чотири", 12), "один два…");
        // Слово довше за ліміт — ріжемо як є, інакше лишиться порожньо.
        assert_eq!(cut("незвичайнодовгеслово", 8), "незвичай…");
    }

    #[test]
    fn strip_tags_leaves_text() {
        assert_eq!(strip_tags("<b>жир</b>ний"), "жирний");
        assert_eq!(strip_tags("<img src=x onerror=alert(1)>"), "");
    }

    #[test]
    fn capitalize_handles_cyrillic_and_empty() {
        assert_eq!(capitalize("привіт"), "Привіт");
        assert_eq!(capitalize(""), "");
    }
}
