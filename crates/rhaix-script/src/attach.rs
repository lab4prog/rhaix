//! `db.attach` і `db.attach_count` — пов'язані записи одним запитом.
//!
//! ```rhai
//! let deals = db.find("deals", #{}, #{ limit: 25 });
//! deals = db.attach(deals, "companies", "company_id", "company");
//! deals = db.attach_count(deals, "contacts", "company_id", "contacts");
//! ```
//!
//! ```html
//! <td>{{ d.company.name }}</td> <td>{{ d.contacts }}</td>
//! ```
//!
//! Найчастіша причина повільної сторінки — запит на кожен рядок таблиці:
//! `db.get("companies", d.company_id)` у `@for` дає 25 запитів замість одного.
//! На SQLite це майже непомітно, а на PostgreSQL кожен запит — похід по
//! мережі: у CRM на rhaix сторінка клієнтів відповідала 185 мс замість 8.
//! Тут пов'язані записи беруться одним `where id in (…)` і розкладаються по
//! рядках.

use std::collections::{BTreeMap, BTreeSet};

use rhai::{Array, Dynamic, Engine, EvalAltResult, Map};
use rhaix_db::{ident, Database, DbError};

type Error = Box<EvalAltResult>;

/// Скільки значень в одному `in (…)`: SQLite має межу на кількість
/// параметрів, і довгий список краще розбити, ніж упертись у неї.
const CHUNK: usize = 500;

fn fail(err: DbError) -> Error {
    err.to_string().into()
}

/// Ключ для зіставлення: `7` і `"7"` — той самий запис.
fn key(value: &Dynamic) -> String {
    crate::display(value)
}

/// Різні значення зовнішнього ключа з рядків, без `()`.
fn foreign_keys(rows: &Array, fk: &str) -> Result<Vec<Dynamic>, Error> {
    let mut seen = BTreeSet::new();
    let mut values = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let Some(map) = row.read_lock::<Map>() else {
            return Err(format!("db.attach: рядок {} — не запис із бази", index + 1).into());
        };
        let Some(value) = map.get(fk) else { continue };
        if value.is_unit() {
            continue;
        }
        if seen.insert(key(value)) {
            values.push(value.clone());
        }
    }
    Ok(values)
}

/// Покласти в кожен рядок поле `as` за значенням його `fk`.
fn place(
    rows: Array,
    fk: &str,
    as_field: &str,
    found: &BTreeMap<String, Dynamic>,
    missing: Dynamic,
) -> Array {
    rows.into_iter()
        .map(|row| {
            let mut map = row.cast::<Map>();
            let value = map
                .get(fk)
                .and_then(|v| found.get(&key(v)).cloned())
                .unwrap_or_else(|| missing.clone());
            map.insert(as_field.into(), value);
            Dynamic::from_map(map)
        })
        .collect()
}

/// `db.attach(rows, table, fk, as)`: `row[as]` = запис із `table`, чий `id`
/// дорівнює `row[fk]`, або `()`.
pub fn attach(
    db: &Database,
    rows: Array,
    table: &str,
    fk: &str,
    as_field: &str,
) -> Result<Array, Error> {
    let ids = foreign_keys(&rows, fk)?;
    let mut found = BTreeMap::new();
    for chunk in ids.chunks(CHUNK) {
        let mut filter = Map::new();
        let mut within = Map::new();
        within.insert("in".into(), Dynamic::from_array(chunk.to_vec()));
        filter.insert(rhaix_db::PRIMARY_KEY.into(), Dynamic::from_map(within));
        for record in db
            .driver()
            .find(table, &filter, &Map::new())
            .map_err(fail)?
        {
            if let Some(id) = record.get(rhaix_db::PRIMARY_KEY) {
                found.insert(key(id), Dynamic::from_map(record.clone()));
            }
        }
    }
    Ok(place(rows, fk, as_field, &found, Dynamic::UNIT))
}

/// `db.attach_count(rows, table, fk, as)`: `row[as]` = скільки записів у
/// `table` мають `fk` = `row.id`. Одним `group by`, а не `count` на рядок.
pub fn attach_count(
    db: &Database,
    rows: Array,
    table: &str,
    fk: &str,
    as_field: &str,
) -> Result<Array, Error> {
    let ids = foreign_keys(&rows, rhaix_db::PRIMARY_KEY)?;
    let table_sql = ident(table).map_err(fail)?;
    let fk_sql = ident(fk).map_err(fail)?;
    let mut found = BTreeMap::new();
    for chunk in ids.chunks(CHUNK) {
        let marks = vec!["?"; chunk.len()].join(", ");
        let sql = format!(
            "select {fk_sql} as k, count(*) as n from {table_sql} where {fk_sql} in ({marks}) group by {fk_sql}"
        );
        for record in db.driver().raw_query(&sql, chunk).map_err(fail)? {
            if let (Some(k), Some(n)) = (record.get("k"), record.get("n")) {
                found.insert(key(k), n.clone());
            }
        }
    }
    // Рядків без пов'язаних записів у `group by` немає — для них 0, а не ().
    Ok(place(
        rows,
        rhaix_db::PRIMARY_KEY,
        as_field,
        &found,
        Dynamic::from(0_i64),
    ))
}

pub fn register_attach(engine: &mut Engine) {
    engine
        .register_fn(
            "attach",
            |db: &mut Database, rows: Array, table: &str, fk: &str, as_field: &str| {
                attach(db, rows, table, fk, as_field)
            },
        )
        .register_fn(
            "attach_count",
            |db: &mut Database, rows: Array, table: &str, fk: &str, as_field: &str| {
                attach_count(db, rows, table, fk, as_field)
            },
        );
}

#[cfg(test)]
mod tests {
    use crate::{engine, Limits};
    use rhai::{Array, Dynamic, Map, Scope};
    use rhaix_db::Database;

    fn database() -> Database {
        let db = Database::open("sqlite", ":memory:").expect("база");
        let d = db.driver();
        for sql in [
            "create table companies (id integer primary key, name text)",
            "create table deals (id integer primary key, company_id integer, title text)",
            "insert into companies (id, name) values (1, 'Альфа'), (2, 'Бета'), (3, 'Гама')",
            "insert into deals (company_id, title) values (1, 'a'), (1, 'b'), (2, 'c'), (null, 'd'), (99, 'e')",
        ] {
            d.raw_exec(sql, &[]).expect(sql);
        }
        db
    }

    fn run(script: &str) -> Array {
        let engine = engine(Limits::default());
        let mut scope = Scope::new();
        scope.push("db", database());
        engine
            .eval_with_scope::<Array>(&mut scope, script)
            .unwrap_or_else(|err| panic!("{err}"))
    }

    fn field(row: &Dynamic, name: &str) -> Dynamic {
        row.clone()
            .cast::<Map>()
            .get(name)
            .cloned()
            .unwrap_or(Dynamic::UNIT)
    }

    #[test]
    fn attach_puts_the_related_record_into_each_row() {
        let rows = run(r#"let deals = db.find("deals", #{}, #{ sort: "id" });
               db.attach(deals, "companies", "company_id", "company")"#);
        let names: Vec<String> = rows
            .iter()
            .map(|r| {
                let company = field(r, "company");
                if company.is_unit() {
                    "—".to_owned()
                } else {
                    field(&company, "name").to_string()
                }
            })
            .collect();
        // Порожній ключ і неіснуючий запис — `()`, а не помилка.
        assert_eq!(names, vec!["Альфа", "Альфа", "Бета", "—", "—"]);
    }

    #[test]
    fn attach_count_counts_in_one_group_by_and_gives_zero_not_unit() {
        let rows = run(
            r#"let companies = db.find("companies", #{}, #{ sort: "id" });
               db.attach_count(companies, "deals", "company_id", "deals")"#,
        );
        let counts: Vec<i64> = rows
            .iter()
            .map(|r| field(r, "deals").as_int().unwrap())
            .collect();
        assert_eq!(counts, vec![2, 1, 0]);
    }

    #[test]
    fn a_bad_table_or_column_name_is_refused() {
        let engine = engine(Limits::default());
        let mut scope = Scope::new();
        scope.push("db", database());
        let err = engine
            .eval_with_scope::<Array>(
                &mut scope,
                r#"db.attach_count(db.find("companies"), "deals; drop table deals", "company_id", "n")"#,
            )
            .unwrap_err();
        assert!(err.to_string().contains("недопустиме ім'я"), "{err}");
    }
}
