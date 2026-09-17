//! Дати й час без зовнішнього крейта.
//!
//! Потрібно рівно три речі: узяти «зараз», перевести мітку часу в поля
//! календаря і назад, показати за шаблоном. Усе це — сто рядків відомих
//! алгоритмів, і воно не варте залежності з базою часових поясів.
//!
//! Зона: усередині все в UTC. Показ зсувається на `tz_offset` із `rhaix.toml`
//! (`[app] tz_offset = "+03:00"`). Літнього часу тут немає — і це чесно
//! написано в доках: для застосунку, який показує «створено о 14:30», зсуву
//! достатньо, а кому потрібні справжні зони, той бере їх у базі.

use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rhai::{Dynamic, Engine};

/// Зсув показу від UTC у хвилинах. Ставиться один раз при старті сервера.
static TZ_OFFSET: AtomicI32 = AtomicI32::new(0);

pub fn set_tz_offset(minutes: i32) {
    TZ_OFFSET.store(minutes, Ordering::Relaxed);
}

pub fn tz_offset() -> i32 {
    TZ_OFFSET.load(Ordering::Relaxed)
}

/// Розібрати `+03:00`, `-0530`, `+3`, `UTC` → хвилини.
pub fn parse_tz_offset(text: &str) -> Option<i32> {
    let text = text.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("utc") || text == "Z" {
        return Some(0);
    }
    let (sign, rest) = match text.as_bytes()[0] {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => (1, text),
    };
    let (hours, minutes) = match rest.split_once(':') {
        Some((h, m)) => (h.parse::<i32>().ok()?, m.parse::<i32>().ok()?),
        None if rest.len() == 4 => (rest[..2].parse().ok()?, rest[2..].parse().ok()?),
        None => (rest.parse::<i32>().ok()?, 0),
    };
    if !(0..=14).contains(&hours) || !(0..60).contains(&minutes) {
        return None;
    }
    Some(sign * (hours * 60 + minutes))
}

/// Секунди від епохи. Час до 1970 року нас не цікавить, але й не ламає.
pub fn now_secs() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(delta) => delta.as_secs() as i64,
        Err(err) => -(err.duration().as_secs() as i64),
    }
}

pub fn now_millis() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(delta) => delta.as_millis() as i64,
        Err(err) => -(err.duration().as_millis() as i64),
    }
}

/// Розкладена мітка часу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parts {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// 0 — неділя.
    pub weekday: u32,
}

/// Мітка часу → поля календаря. Алгоритм Говарда Гіннанта (civil_from_days).
pub fn parts(timestamp: i64) -> Parts {
    let days = timestamp.div_euclid(86_400);
    let rest = timestamp.rem_euclid(86_400);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;

    Parts {
        year: if month <= 2 { year + 1 } else { year },
        month,
        day,
        hour: (rest / 3600) as u32,
        minute: (rest % 3600 / 60) as u32,
        second: (rest % 60) as u32,
        // 1970-01-01 — четвер, тому зсув на 4.
        weekday: (days + 4).rem_euclid(7) as u32,
    }
}

/// Поля календаря → мітка часу (days_from_civil).
pub fn timestamp(year: i64, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if month > 2 { month - 3 } else { month + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    days * 86_400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64
}

/// Показати мітку часу за шаблоном у стилі moment.js.
///
/// `YYYY YY MM M DD D HH H mm m ss s` — і все. Ніяких `%d` і `%Y`: людина,
/// яка не програміст, читає `DD.MM.YYYY` без довідника.
/// Літерали в лапках лишаються як є: `"о" HH:mm`.
pub fn format(timestamp: i64, pattern: &str) -> String {
    let p = parts(timestamp + tz_offset() as i64 * 60);
    let mut out = String::with_capacity(pattern.len() + 8);
    let bytes = pattern.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'"' {
            // Літерал: усе до наступної лапки йде дослівно.
            if let Some(end) = pattern[i + 1..].find('"') {
                out.push_str(&pattern[i + 1..i + 1 + end]);
                i += end + 2;
                continue;
            }
        }
        let rest = &pattern[i..];
        let matched = [
            ("YYYY", format!("{:04}", p.year)),
            ("YY", format!("{:02}", p.year.rem_euclid(100))),
            ("MM", format!("{:02}", p.month)),
            ("DD", format!("{:02}", p.day)),
            ("HH", format!("{:02}", p.hour)),
            ("mm", format!("{:02}", p.minute)),
            ("ss", format!("{:02}", p.second)),
            ("M", p.month.to_string()),
            ("D", p.day.to_string()),
            ("H", p.hour.to_string()),
            ("m", p.minute.to_string()),
            ("s", p.second.to_string()),
        ]
        .into_iter()
        .find(|(token, _)| rest.starts_with(token));

        match matched {
            Some((token, value)) => {
                out.push_str(&value);
                i += token.len();
            }
            None => {
                // Не токен — просто символ. Ідемо по символах, щоб не
                // розрізати кирилицю навпіл.
                let ch = pattern[i..].chars().next().expect("рядок не порожній");
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

/// Розібрати `YYYY-MM-DD`, `YYYY-MM-DD HH:MM[:SS]`, ISO-8601 із `T` і `Z`.
///
/// Саме в такому вигляді дати повертає SQLite, тому ця функція — місток між
/// базою й `date()`.
pub fn parse(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    // Чисте число — це вже мітка часу.
    if let Ok(number) = text.parse::<i64>() {
        return Some(number);
    }

    let (date_part, rest) = match text.find(['T', ' ']) {
        Some(index) => (&text[..index], &text[index + 1..]),
        None => (text, ""),
    };
    let mut date = date_part.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: u32 = date.next()?.parse().ok()?;
    let day: u32 = date.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // Зона в хвості: `Z`, `+03:00`. Її ще треба відняти, щоб вийшов UTC.
    let (time_part, zone) = split_zone(rest);
    let mut time = time_part.split(':');
    let hour: u32 = time.next().unwrap_or("0").trim().parse().unwrap_or(0);
    let minute: u32 = time.next().unwrap_or("0").parse().unwrap_or(0);
    let second: u32 = time
        .next()
        .unwrap_or("0")
        .split('.')
        .next()
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);

    Some(timestamp(year, month, day, hour, minute, second) - zone as i64 * 60)
}

fn split_zone(rest: &str) -> (&str, i32) {
    let rest = rest.trim();
    if let Some(stripped) = rest.strip_suffix('Z').or_else(|| rest.strip_suffix('z')) {
        return (stripped, 0);
    }
    // Шукаємо знак зони після часу: `10:00+03:00`.
    if let Some(index) = rest.rfind(['+', '-']) {
        if index > 0 {
            if let Some(offset) = parse_tz_offset(&rest[index..]) {
                return (&rest[..index], offset);
            }
        }
    }
    (rest, 0)
}

/// Значення, з якого можна дістати мітку часу: число або рядок із бази.
fn as_timestamp(value: &Dynamic) -> Option<i64> {
    if let Ok(number) = value.as_int() {
        return Some(number);
    }
    if let Ok(number) = value.as_float() {
        return Some(number as i64);
    }
    value.clone().into_string().ok().as_deref().and_then(parse)
}

pub fn register_datetime(engine: &mut Engine) {
    engine
        .register_fn("now", now_secs)
        .register_fn("now_ms", now_millis)
        .register_fn("today", || format(now_secs(), "YYYY-MM-DD"))
        // `date(value)` і `date(value, шаблон)`: value — мітка часу або рядок
        // із бази. Те, що не схоже на дату, повертається як порожній рядок:
        // сторінка не має падати через кривий запис у колонці.
        .register_fn("date", |value: Dynamic| match as_timestamp(&value) {
            Some(ts) => format(ts, "YYYY-MM-DD"),
            None => String::new(),
        })
        .register_fn("date", |value: Dynamic, pattern: &str| {
            match as_timestamp(&value) {
                Some(ts) => format(ts, pattern),
                None => String::new(),
            }
        })
        .register_fn("datetime", |value: Dynamic| match as_timestamp(&value) {
            Some(ts) => format(ts, "YYYY-MM-DD HH:mm:ss"),
            None => String::new(),
        })
        .register_fn("timestamp", |text: &str| match parse(text) {
            Some(ts) => Dynamic::from(ts),
            None => Dynamic::UNIT,
        })
        // Арифметика в термінах, у яких думає людина.
        .register_fn("days", |count: i64| count * 86_400)
        .register_fn("hours", |count: i64| count * 3_600)
        .register_fn("minutes", |count: i64| count * 60);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_round_trip() {
        for stamp in [0, 1, 86_399, 951_782_400, 1_700_000_000, -86_400] {
            let p = parts(stamp);
            assert_eq!(
                timestamp(p.year, p.month, p.day, p.hour, p.minute, p.second),
                stamp,
                "{stamp} → {p:?}"
            );
        }
    }

    #[test]
    fn known_dates_are_right() {
        let p = parts(0);
        assert_eq!((p.year, p.month, p.day, p.weekday), (1970, 1, 1, 4));
        // 2000-02-29 — високосний рік у столітті, класичний зріз алгоритму.
        let leap = timestamp(2000, 2, 29, 12, 0, 0);
        let p = parts(leap);
        assert_eq!((p.year, p.month, p.day, p.hour), (2000, 2, 29, 12));
    }

    #[test]
    fn format_uses_readable_tokens() {
        let stamp = timestamp(2026, 9, 17, 14, 5, 9);
        assert_eq!(format(stamp, "DD.MM.YYYY"), "17.09.2026");
        assert_eq!(format(stamp, "D.M.YY H:mm"), "17.9.26 14:05");
        assert_eq!(format(stamp, "YYYY-MM-DD HH:mm:ss"), "2026-09-17 14:05:09");
        // Кирилиця в шаблоні не ріжеться і не плутається з токенами.
        assert_eq!(format(stamp, "DD.MM о HH:mm"), "17.09 о 14:05");
    }

    #[test]
    fn quoted_text_is_left_alone() {
        let stamp = timestamp(2026, 9, 17, 14, 5, 0);
        assert_eq!(format(stamp, r#""Day" D"#), "Day 17");
    }

    #[test]
    fn parses_what_sqlite_returns() {
        assert_eq!(parse("2026-09-17"), Some(timestamp(2026, 9, 17, 0, 0, 0)));
        assert_eq!(
            parse("2026-09-17 14:05:09"),
            Some(timestamp(2026, 9, 17, 14, 5, 9))
        );
        assert_eq!(
            parse("2026-09-17T14:05:09Z"),
            Some(timestamp(2026, 9, 17, 14, 5, 9))
        );
        // Зона віднімається, щоб вийшов UTC.
        assert_eq!(
            parse("2026-09-17T14:05:09+03:00"),
            Some(timestamp(2026, 9, 17, 11, 5, 9))
        );
        assert_eq!(parse("сьогодні"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn timezone_offsets_are_parsed() {
        assert_eq!(parse_tz_offset("+03:00"), Some(180));
        assert_eq!(parse_tz_offset("-05:30"), Some(-330));
        assert_eq!(parse_tz_offset("+0200"), Some(120));
        assert_eq!(parse_tz_offset("UTC"), Some(0));
        assert_eq!(parse_tz_offset("+99:00"), None);
        assert_eq!(parse_tz_offset("Київ"), None);
    }
}
