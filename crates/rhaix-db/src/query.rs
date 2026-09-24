//! Переносимий CRUD → SQL.
//!
//! Сюди зведене все, що перетворює `db.find("orders", #{ qty: #{ gte: 10 } })`
//! на параметризований запит. Жодне значення не потрапляє в текст SQL: усе йде
//! плейсхолдерами, а імена таблиць і колонок проходять сувору перевірку —
//! інакше переносимий шар сам став би діркою для ін'єкції.

use rhai::{Array, Dynamic, Map};

use crate::DbError;

/// Готовий запит: текст плюс значення до нього.
#[derive(Debug, Clone)]
pub struct Sql {
    pub text: String,
    pub params: Vec<Dynamic>,
}

/// Колонка, за якою шукають запис у `get`/`update`/`delete`.
pub const PRIMARY_KEY: &str = "id";

/// Перевірити ім'я таблиці або колонки.
///
/// Дозволені лише літери, цифри й підкреслення. Усе інше — помилка, а не
/// екранування: якщо в імені колонки є лапки, це майже напевно чиясь спроба
/// щось підсунути.
pub fn ident(name: &str) -> Result<String, DbError> {
    let valid = !name.is_empty()
        && name
            .chars()
            .next()
            .map(|ch| ch.is_ascii_alphabetic() || ch == '_')
            .unwrap_or(false)
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_');

    if valid {
        Ok(format!("\"{name}\""))
    } else {
        Err(DbError::Query(format!(
            "недопустиме ім'я `{name}`: дозволені літери, цифри й підкреслення"
        )))
    }
}

/// Ключі, які розуміє `options` у `db.find`.
const OPTION_KEYS: [&str; 4] = ["sort", "limit", "skip", "fields"];

/// Невідомий ключ в `options` — помилка, а не мовчазне ігнорування.
///
/// Знайдено на власному рецепті пагінації: `#{ offset: 20 }` замість `skip`
/// просто зникав, і сторінка показувала перші записи замість других. Сторінка
/// при цьому виглядала цілком справною — найгірший різновид помилки.
fn check_options(options: &Map) -> Result<(), DbError> {
    for key in options.keys() {
        if !OPTION_KEYS.contains(&key.as_str()) {
            return Err(DbError::Query(format!(
                "невідомий параметр `{key}`; доступні: {}",
                OPTION_KEYS.join(", ")
            )));
        }
    }
    Ok(())
}

/// `db.find(table, filter, options)`
pub fn find(table: &str, filter: &Map, options: &Map) -> Result<Sql, DbError> {
    check_options(options)?;
    let mut params = Vec::new();
    let columns = select_list(options)?;
    let mut text = format!("select {columns} from {}", ident(table)?);

    let where_clause = where_clause(filter, &mut params)?;
    if !where_clause.is_empty() {
        text.push_str(" where ");
        text.push_str(&where_clause);
    }
    if let Some(order) = order_by(options)? {
        text.push_str(" order by ");
        text.push_str(&order);
    }
    if let Some(limit) = int_option(options, "limit") {
        text.push_str(&format!(" limit {limit}"));
    }
    if let Some(skip) = int_option(options, "skip") {
        // SQLite без limit не приймає offset — ставимо свідомо великий ліміт
        if int_option(options, "limit").is_none() {
            text.push_str(" limit -1");
        }
        text.push_str(&format!(" offset {skip}"));
    }

    Ok(Sql { text, params })
}

/// `db.count(table, filter)`
pub fn count(table: &str, filter: &Map) -> Result<Sql, DbError> {
    let mut params = Vec::new();
    let mut text = format!("select count(*) as n from {}", ident(table)?);
    let where_clause = where_clause(filter, &mut params)?;
    if !where_clause.is_empty() {
        text.push_str(" where ");
        text.push_str(&where_clause);
    }
    Ok(Sql { text, params })
}

/// `db.get(table, id)`
pub fn get(table: &str, id: Dynamic) -> Result<Sql, DbError> {
    Ok(Sql {
        text: format!(
            "select * from {} where {} = ? limit 1",
            ident(table)?,
            ident(PRIMARY_KEY)?
        ),
        params: vec![id],
    })
}

/// `db.insert(table, values)`
pub fn insert(table: &str, values: &Map) -> Result<Sql, DbError> {
    if values.is_empty() {
        return Err(DbError::Query("немає що вставляти: порожня мапа".into()));
    }
    let mut columns = Vec::new();
    let mut params = Vec::new();
    for (key, value) in values.iter() {
        columns.push(ident(key)?);
        params.push(value.clone());
    }
    let placeholders = vec!["?"; columns.len()].join(", ");
    Ok(Sql {
        text: format!(
            "insert into {} ({}) values ({placeholders})",
            ident(table)?,
            columns.join(", ")
        ),
        params,
    })
}

/// `db.update(table, id, values)`
pub fn update(table: &str, id: Dynamic, values: &Map) -> Result<Sql, DbError> {
    if values.is_empty() {
        return Err(DbError::Query("немає що оновлювати: порожня мапа".into()));
    }
    let mut assignments = Vec::new();
    let mut params = Vec::new();
    for (key, value) in values.iter() {
        assignments.push(format!("{} = ?", ident(key)?));
        params.push(value.clone());
    }
    params.push(id);
    Ok(Sql {
        text: format!(
            "update {} set {} where {} = ?",
            ident(table)?,
            assignments.join(", "),
            ident(PRIMARY_KEY)?
        ),
        params,
    })
}

/// `db.delete(table, id)`
pub fn delete(table: &str, id: Dynamic) -> Result<Sql, DbError> {
    Ok(Sql {
        text: format!(
            "delete from {} where {} = ?",
            ident(table)?,
            ident(PRIMARY_KEY)?
        ),
        params: vec![id],
    })
}

// ------------------------------------------------------------------ деталі

fn select_list(options: &Map) -> Result<String, DbError> {
    let Some(fields) = options.get("fields") else {
        return Ok("*".to_owned());
    };
    let Some(array) = fields.read_lock::<Array>() else {
        return Err(DbError::Query("`fields` має бути масивом колонок".into()));
    };
    let mut columns = Vec::new();
    for field in array.iter() {
        columns.push(ident(&field.to_string())?);
    }
    if columns.is_empty() {
        return Ok("*".to_owned());
    }
    Ok(columns.join(", "))
}

/// `"created desc"` або `"dept asc, created desc"`.
fn order_by(options: &Map) -> Result<Option<String>, DbError> {
    let Some(sort) = options.get("sort") else {
        return Ok(None);
    };
    let text = sort.to_string();
    if text.trim().is_empty() {
        return Ok(None);
    }

    let mut parts = Vec::new();
    for piece in text.split(',') {
        let mut words = piece.split_whitespace();
        let Some(column) = words.next() else { continue };
        let direction = match words.next().map(|w| w.to_ascii_lowercase()) {
            None => "asc".to_owned(),
            Some(word) if word == "asc" || word == "desc" => word,
            Some(word) => {
                return Err(DbError::Query(format!(
                    "напрямок сортування має бути `asc` або `desc`, а не `{word}`"
                )))
            }
        };
        if words.next().is_some() {
            return Err(DbError::Query(format!(
                "незрозуміле сортування: `{}`",
                piece.trim()
            )));
        }
        parts.push(format!("{} {direction}", ident(column)?));
    }
    Ok(Some(parts.join(", ")))
}

fn int_option(options: &Map, name: &str) -> Option<i64> {
    options.get(name).and_then(|value| value.as_int().ok())
}

/// Фільтр → умова. Порожній фільтр дає порожній рядок.
fn where_clause(filter: &Map, params: &mut Vec<Dynamic>) -> Result<String, DbError> {
    let mut conditions = Vec::new();
    for (key, value) in filter.iter() {
        let column = ident(key)?;

        // Мапа означає оператори: `#{ qty: #{ gte: 10, lt: 100 } }`
        if let Some(operators) = value.read_lock::<Map>() {
            for (operator, operand) in operators.iter() {
                conditions.push(condition(&column, operator, operand, params)?);
            }
            continue;
        }

        if value.is_unit() {
            conditions.push(format!("{column} is null"));
            continue;
        }
        conditions.push(format!("{column} = ?"));
        params.push(value.clone());
    }
    Ok(conditions.join(" and "))
}

fn condition(
    column: &str,
    operator: &str,
    operand: &Dynamic,
    params: &mut Vec<Dynamic>,
) -> Result<String, DbError> {
    let simple = |sign: &str, params: &mut Vec<Dynamic>| {
        params.push(operand.clone());
        format!("{column} {sign} ?")
    };

    let clause = match operator {
        "eq" => simple("=", params),
        "ne" => simple("<>", params),
        "gt" => simple(">", params),
        "gte" => simple(">=", params),
        "lt" => simple("<", params),
        "lte" => simple("<=", params),
        // Пошук — без урахування регістру, і однаково на обох драйверах.
        // Голий `like` у SQLite ігнорує регістр лише латиниці, а в Postgres —
        // не ігнорує зовсім: «мед» не знаходив «МедСервіс» ніде, а той самий
        // `.rhx` поводився по-різному залежно від бази. Рядок пошуку знижуємо
        // тут (Unicode), колонку — `lower()` (у SQLite драйвер підміняє його
        // Unicode-версією). `cast` — щоб і числова колонка шукалась як текст.
        "contains" => {
            params.push(Dynamic::from(format!("%{}%", escape_like(operand))));
            format!("lower(cast({column} as text)) like ? escape '\\'")
        }
        "starts" => {
            params.push(Dynamic::from(format!("{}%", escape_like(operand))));
            format!("lower(cast({column} as text)) like ? escape '\\'")
        }
        "ends" => {
            params.push(Dynamic::from(format!("%{}", escape_like(operand))));
            format!("lower(cast({column} as text)) like ? escape '\\'")
        }
        "in" | "nin" => {
            let Some(values) = operand.read_lock::<Array>() else {
                return Err(DbError::Query(format!(
                    "оператор `{operator}` очікує масив значень"
                )));
            };
            if values.is_empty() {
                // порожній список: `in []` не знаходить нічого, `nin []` — усе
                return Ok(if operator == "in" { "0 = 1" } else { "1 = 1" }.to_owned());
            }
            let placeholders = vec!["?"; values.len()].join(", ");
            for value in values.iter() {
                params.push(value.clone());
            }
            let sign = if operator == "in" { "in" } else { "not in" };
            format!("{column} {sign} ({placeholders})")
        }
        "between" => {
            let Some(bounds) = operand.read_lock::<Array>() else {
                return Err(DbError::Query(
                    "оператор `between` очікує масив із двох значень".into(),
                ));
            };
            if bounds.len() != 2 {
                return Err(DbError::Query(
                    "оператор `between` очікує рівно два значення".into(),
                ));
            }
            params.push(bounds[0].clone());
            params.push(bounds[1].clone());
            format!("{column} between ? and ?")
        }
        "is_null" => {
            let wanted = operand.as_bool().unwrap_or(true);
            if wanted {
                format!("{column} is null")
            } else {
                format!("{column} is not null")
            }
        }
        other => {
            return Err(DbError::Query(format!(
                "невідомий оператор `{other}`; доступні: eq, ne, gt, gte, lt, lte, \
                 in, nin, contains, starts, ends, between, is_null"
            )))
        }
    };
    Ok(clause)
}

/// `%` і `_` у пошуковому рядку — це шаблон LIKE, а не текст користувача.
fn escape_like(value: &Dynamic) -> String {
    value
        .to_string()
        .to_lowercase()
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, Dynamic)]) -> Map {
        let mut out = Map::new();
        for (key, value) in pairs {
            out.insert((*key).into(), value.clone());
        }
        out
    }

    #[test]
    fn identifiers_are_validated_not_escaped() {
        assert_eq!(ident("orders").unwrap(), "\"orders\"");
        assert_eq!(ident("created_at").unwrap(), "\"created_at\"");
        assert!(ident("orders; drop table users").is_err());
        assert!(ident("\"sneaky\"").is_err());
        assert!(ident("").is_err());
    }

    #[test]
    fn equality_filter_is_parameterised() {
        let sql = find(
            "orders",
            &map(&[("done", Dynamic::from(false))]),
            &Map::new(),
        )
        .unwrap();
        assert_eq!(sql.text, "select * from \"orders\" where \"done\" = ?");
        assert_eq!(sql.params.len(), 1);
    }

    #[test]
    fn operators_compile_to_sql() {
        let filter = map(&[
            (
                "qty",
                Dynamic::from_map(map(&[
                    ("gte", Dynamic::from(10_i64)),
                    ("lt", Dynamic::from(100_i64)),
                ])),
            ),
            (
                "title",
                Dynamic::from_map(map(&[("contains", Dynamic::from("молоко"))])),
            ),
            (
                "closed",
                Dynamic::from_map(map(&[("is_null", Dynamic::from(true))])),
            ),
        ]);
        let sql = find("orders", &filter, &Map::new()).unwrap();

        assert!(sql.text.contains("\"qty\" >= ?"), "{}", sql.text);
        assert!(sql.text.contains("\"qty\" < ?"), "{}", sql.text);
        assert!(
            sql.text.contains("lower(cast(\"title\" as text)) like ?"),
            "{}",
            sql.text
        );
        assert!(sql.text.contains("\"closed\" is null"), "{}", sql.text);
        assert_eq!(sql.params.len(), 3);
        assert_eq!(sql.params[2].to_string(), "%молоко%");
    }

    #[test]
    fn in_operator_handles_empty_lists() {
        let filter = map(&[(
            "status",
            Dynamic::from_map(map(&[("in", Dynamic::from(Array::new()))])),
        )]);
        let sql = find("orders", &filter, &Map::new()).unwrap();
        assert!(sql.text.ends_with("where 0 = 1"), "{}", sql.text);
    }

    #[test]
    fn unknown_operator_lists_the_known_ones() {
        let filter = map(&[(
            "qty",
            Dynamic::from_map(map(&[("approximately", Dynamic::from(1_i64))])),
        )]);
        let err = find("orders", &filter, &Map::new()).unwrap_err();
        assert!(err.to_string().contains("contains"), "{err}");
    }

    #[test]
    fn sort_is_a_string_and_is_validated() {
        let options = map(&[("sort", Dynamic::from("created desc"))]);
        let sql = find("orders", &Map::new(), &options).unwrap();
        assert!(
            sql.text.ends_with("order by \"created\" desc"),
            "{}",
            sql.text
        );

        let options = map(&[("sort", Dynamic::from("created; drop table x"))]);
        assert!(find("orders", &Map::new(), &options).is_err());

        let options = map(&[("sort", Dynamic::from("created sideways"))]);
        assert!(find("orders", &Map::new(), &options).is_err());
    }

    #[test]
    fn an_unknown_option_is_an_error_not_silence() {
        // `offset` замість `skip` колись просто зникав, і пагінація мовчки
        // показувала не ту сторінку.
        let options: Map = [("offset".into(), Dynamic::from(20_i64))]
            .into_iter()
            .collect();
        let err = find("orders", &Map::new(), &options).expect_err("має бути помилка");
        let text = err.to_string();
        assert!(text.contains("offset"), "{text}");
        assert!(text.contains("skip"), "{text}");
    }

    #[test]
    fn limit_and_skip_work_together() {
        let options = map(&[
            ("limit", Dynamic::from(20_i64)),
            ("skip", Dynamic::from(40_i64)),
        ]);
        let sql = find("orders", &Map::new(), &options).unwrap();
        assert!(sql.text.ends_with("limit 20 offset 40"), "{}", sql.text);

        let options = map(&[("skip", Dynamic::from(5_i64))]);
        let sql = find("orders", &Map::new(), &options).unwrap();
        assert!(sql.text.ends_with("limit -1 offset 5"), "{}", sql.text);
    }

    #[test]
    fn writes_are_parameterised_too() {
        let values = map(&[
            ("title", Dynamic::from("нове")),
            ("done", Dynamic::from(false)),
        ]);
        let sql = insert("todos", &values).unwrap();
        assert_eq!(
            sql.text,
            "insert into \"todos\" (\"done\", \"title\") values (?, ?)"
        );
        assert_eq!(sql.params.len(), 2);

        let sql = update("todos", Dynamic::from(7_i64), &values).unwrap();
        assert!(sql.text.starts_with("update \"todos\" set"), "{}", sql.text);
        assert!(sql.text.ends_with("where \"id\" = ?"), "{}", sql.text);
        assert_eq!(sql.params.len(), 3);

        let sql = delete("todos", Dynamic::from(7_i64)).unwrap();
        assert_eq!(sql.text, "delete from \"todos\" where \"id\" = ?");
    }

    #[test]
    fn text_search_ignores_case_including_cyrillic() {
        let filter = map(&[(
            "name",
            Dynamic::from_map(map(&[("contains", Dynamic::from("МЕД"))])),
        )]);
        let sql = find("companies", &filter, &Map::new()).unwrap();
        assert_eq!(sql.params[0].to_string(), "%мед%");
        assert!(
            sql.text.contains("lower(cast(\"name\" as text))"),
            "{}",
            sql.text
        );
    }

    #[test]
    fn like_wildcards_from_user_input_are_escaped() {
        let filter = map(&[(
            "title",
            Dynamic::from_map(map(&[("contains", Dynamic::from("100%_"))])),
        )]);
        let sql = find("orders", &filter, &Map::new()).unwrap();
        assert_eq!(sql.params[0].to_string(), "%100\\%\\_%");
    }
}
