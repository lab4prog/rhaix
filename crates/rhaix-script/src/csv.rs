//! `csv(rows, options)` — таблиця в CSV, який відкриє і Excel, і скрипт.
//!
//! ```rhai
//! let orders = db.find("orders", #{}, #{ sort: "id" });
//! let text = csv(orders, #{
//!     columns: ["id", "customer", "amount"],
//!     titles:  ["№", "Клієнт", "Сума"],
//! });
//! res.download("замовлення.csv", text);
//! ```
//!
//! Склеїти CSV руками здається простим рівно до першої коми в імені клієнта,
//! лапки в назві чи переносу рядка в коментарі — тоді колонки їдуть. Тому
//! екранування тут, за RFC 4180, а не в кожному застосунку.
//!
//! Мапа в Rhai не пам'ятає порядку полів, тому для рядків-мап колонки
//! називаються явно (`columns`). Масив масивів іде як є.

use rhai::{Array, Dynamic, Engine, EvalAltResult, Map, FLOAT, INT};

/// Параметри, розібрані з мапи `options`.
struct Options {
    columns: Option<Vec<String>>,
    titles: Option<Vec<String>>,
    sep: char,
    /// Десятковий знак для дробових чисел: `,` для Excel з українською локаллю.
    decimal: char,
    header: bool,
    guard: bool,
}

type Error = Box<EvalAltResult>;

fn strings(value: &Dynamic, name: &str) -> Result<Vec<String>, Error> {
    let Some(array) = value.clone().try_cast::<Array>() else {
        return Err(format!("csv(): `{name}` має бути масивом рядків").into());
    };
    Ok(array.iter().map(crate::display).collect())
}

fn options(map: &Map) -> Result<Options, Error> {
    let mut options = Options {
        columns: None,
        titles: None,
        sep: ',',
        decimal: '.',
        header: true,
        guard: true,
    };
    for (key, value) in map {
        match key.as_str() {
            "columns" => options.columns = Some(strings(value, "columns")?),
            "titles" => options.titles = Some(strings(value, "titles")?),
            "sep" => {
                let text = crate::display(value);
                let mut chars = text.chars();
                match (chars.next(), chars.next()) {
                    (Some(sep), None) if sep != '"' && sep != '\n' && sep != '\r' => {
                        options.sep = sep
                    }
                    _ => {
                        return Err(format!(
                            "csv(): `sep` — один символ, крім лапки й переносу, а не `{text}`"
                        )
                        .into())
                    }
                }
            }
            "decimal" => {
                let text = crate::display(value);
                match text.as_str() {
                    "." => options.decimal = '.',
                    "," => options.decimal = ',',
                    _ => {
                        return Err(
                            format!("csv(): `decimal` — \".\" або \",\", а не `{text}`").into()
                        )
                    }
                }
            }
            "header" => {
                options.header = value
                    .as_bool()
                    .map_err(|_| -> Error { "csv(): `header` — true або false".into() })?
            }
            "guard" => {
                options.guard = value
                    .as_bool()
                    .map_err(|_| -> Error { "csv(): `guard` — true або false".into() })?
            }
            // Опечатка в назві параметра мовчки дала б «не той» файл.
            other => {
                return Err(format!(
                    "csv(): невідомий параметр `{other}` (є columns, titles, sep, decimal, header, guard)"
                )
                .into())
            }
        }
    }
    if let (Some(columns), Some(titles)) = (&options.columns, &options.titles) {
        if columns.len() != titles.len() {
            return Err(format!(
                "csv(): `titles` має {} назв, а `columns` — {}",
                titles.len(),
                columns.len()
            )
            .into());
        }
    }
    Ok(options)
}

/// Значення комірки як текст.
fn cell_text(value: &Dynamic, decimal: char) -> String {
    if value.is_unit() {
        return String::new();
    }
    if let Ok(number) = value.as_int() {
        return number.to_string();
    }
    if let Ok(number) = value.as_float() {
        // `250.0` → `250`, `250.5` → `250.5`: так, як число пишуть люди.
        let text = format!("{number}");
        return if decimal == '.' {
            text
        } else {
            text.replace('.', &decimal.to_string())
        };
    }
    if value.is::<Map>() || value.is::<Array>() {
        return crate::json_encode(value);
    }
    crate::display(value)
}

/// Одна комірка: захист від формул і лапки там, де без них CSV розвалиться.
fn cell(value: &Dynamic, options: &Options) -> String {
    let mut text = cell_text(value, options.decimal);
    // CSV-ін'єкція: Excel виконує комірку, що починається з `=`, `+`, `-`,
    // `@`, як формулу — і `=HYPERLINK(...)` в імені клієнта стає посиланням на
    // чужий сайт у файлі бухгалтера. Числа не чіпаємо: `-5` із бази — це число,
    // а не текст від користувача.
    let is_number = value.is::<INT>() || value.is::<FLOAT>();
    if options.guard && !is_number && text.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        text.insert(0, '\'');
    }
    let needs_quotes = text.contains(options.sep)
        || text.contains('"')
        || text.contains('\n')
        || text.contains('\r');
    if needs_quotes {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    }
}

fn line(cells: impl Iterator<Item = String>, sep: char) -> String {
    let mut out = String::new();
    for (index, cell) in cells.enumerate() {
        if index > 0 {
            out.push(sep);
        }
        out.push_str(&cell);
    }
    out.push_str("\r\n");
    out
}

pub fn csv(rows: &Array, options_map: &Map) -> Result<String, Error> {
    let options = options(options_map)?;
    let mut out = String::new();

    if options.header {
        let header = options.titles.as_ref().or(options.columns.as_ref());
        if let Some(header) = header {
            let cells = header
                .iter()
                .map(|title| cell(&Dynamic::from(title.clone()), &options));
            out.push_str(&line(cells, options.sep));
        }
    }

    for (index, row) in rows.iter().enumerate() {
        if let Some(map) = row.read_lock::<Map>() {
            let Some(columns) = &options.columns else {
                return Err(
                    "csv(): для рядків-мап потрібен `columns` — мапа в Rhai не пам'ятає \
                     порядку полів"
                        .into(),
                );
            };
            let cells = columns.iter().map(|column| {
                let value = map.get(column.as_str()).cloned().unwrap_or(Dynamic::UNIT);
                cell(&value, &options)
            });
            out.push_str(&line(cells, options.sep));
        } else if let Some(array) = row.read_lock::<Array>() {
            out.push_str(&line(array.iter().map(|v| cell(v, &options)), options.sep));
        } else {
            return Err(format!(
                "csv(): рядок {} — не мапа й не масив, а `{}`",
                index + 1,
                row.type_name()
            )
            .into());
        }
    }
    Ok(out)
}

pub fn register_csv(engine: &mut Engine) {
    engine
        .register_fn("csv", |rows: Array| csv(&rows, &Map::new()))
        .register_fn("csv", |rows: Array, options: Map| csv(&rows, &options));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pairs: &[(&str, Dynamic)]) -> Dynamic {
        let mut map = Map::new();
        for (key, value) in pairs {
            map.insert((*key).into(), value.clone());
        }
        Dynamic::from_map(map)
    }

    fn opts(pairs: &[(&str, Dynamic)]) -> Map {
        row(pairs).cast::<Map>()
    }

    fn strs(items: &[&str]) -> Dynamic {
        Dynamic::from_array(items.iter().map(|s| Dynamic::from(s.to_string())).collect())
    }

    #[test]
    fn maps_follow_the_named_columns_and_titles() {
        let rows = vec![
            row(&[
                ("id", Dynamic::from(1_i64)),
                ("customer", Dynamic::from("Оля".to_owned())),
                ("amount", Dynamic::from(250.0_f64)),
            ]),
            row(&[
                ("id", Dynamic::from(2_i64)),
                ("customer", Dynamic::from("Петро".to_owned())),
                ("amount", Dynamic::from(99.5_f64)),
            ]),
        ];
        let text = csv(
            &rows,
            &opts(&[
                ("columns", strs(&["id", "customer", "amount"])),
                ("titles", strs(&["№", "Клієнт", "Сума"])),
            ]),
        )
        .unwrap();
        assert_eq!(text, "№,Клієнт,Сума\r\n1,Оля,250\r\n2,Петро,99.5\r\n");
    }

    #[test]
    fn separators_quotes_and_newlines_are_quoted() {
        let rows = vec![Dynamic::from_array(vec![
            Dynamic::from("ТОВ \"Ромашка\", Київ".to_owned()),
            Dynamic::from("рядок 1\nрядок 2".to_owned()),
            Dynamic::UNIT,
        ])];
        let text = csv(&rows, &Map::new()).unwrap();
        assert_eq!(
            text,
            "\"ТОВ \"\"Ромашка\"\", Київ\",\"рядок 1\nрядок 2\",\r\n"
        );
    }

    #[test]
    fn text_that_looks_like_a_formula_is_defused_but_numbers_are_not() {
        let rows = vec![Dynamic::from_array(vec![
            Dynamic::from("=HYPERLINK(\"http://evil\")".to_owned()),
            Dynamic::from("@SUM(A1)".to_owned()),
            Dynamic::from(-5_i64),
            Dynamic::from(-2.5_f64),
        ])];
        let text = csv(&rows, &Map::new()).unwrap();
        assert_eq!(
            text,
            "\"'=HYPERLINK(\"\"http://evil\"\")\",'@SUM(A1),-5,-2.5\r\n"
        );

        // Хто знає, що робить, вимикає захист явно.
        let text = csv(&rows, &opts(&[("guard", Dynamic::from(false))])).unwrap();
        assert!(text.starts_with("\"=HYPERLINK"), "{text}");
    }

    #[test]
    fn semicolon_for_excel_with_a_comma_decimal_locale() {
        let rows = vec![Dynamic::from_array(vec![
            Dynamic::from("а,б".to_owned()),
            Dynamic::from(1_i64),
        ])];
        let text = csv(&rows, &opts(&[("sep", Dynamic::from(";".to_owned()))])).unwrap();
        // Кома більше не роздільник — лапки не потрібні.
        assert_eq!(text, "а,б;1\r\n");
    }

    #[test]
    fn maps_without_columns_and_typos_are_errors_not_silence() {
        let rows = vec![row(&[("id", Dynamic::from(1_i64))])];
        assert!(csv(&rows, &Map::new()).is_err());
        assert!(csv(&rows, &opts(&[("colums", strs(&["id"]))])).is_err());
        assert!(csv(
            &rows,
            &opts(&[("columns", strs(&["id"])), ("titles", strs(&["a", "b"]))])
        )
        .is_err());
        assert!(csv(&rows, &opts(&[("sep", Dynamic::from(";;".to_owned()))])).is_err());
    }

    #[test]
    fn decimal_comma_for_excel_with_a_ukrainian_locale() {
        let rows = vec![Dynamic::from_array(vec![
            Dynamic::from(380.5_f64),
            Dynamic::from(1240_i64),
            Dynamic::from("1.5 кг".to_owned()),
        ])];
        let text = csv(
            &rows,
            &opts(&[
                ("sep", Dynamic::from(";".to_owned())),
                ("decimal", Dynamic::from(",".to_owned())),
            ]),
        )
        .unwrap();
        // Лише числа: текст із крапкою лишається як був.
        assert_eq!(text, "380,5;1240;1.5 кг\r\n");

        // З комою-роздільником дробове число в лапках — інакше розвалилось би.
        let text = csv(&rows, &opts(&[("decimal", Dynamic::from(",".to_owned()))])).unwrap();
        assert!(text.starts_with("\"380,5\","), "{text}");
    }

    #[test]
    fn header_can_be_switched_off() {
        let rows = vec![row(&[("id", Dynamic::from(7_i64))])];
        let text = csv(
            &rows,
            &opts(&[("columns", strs(&["id"])), ("header", Dynamic::from(false))]),
        )
        .unwrap();
        assert_eq!(text, "7\r\n");
    }
}
