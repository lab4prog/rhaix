//! `db.grid(table, req, options)` — таблиця для адмінки одним викликом.
//!
//! ```rhai
//! let g = db.grid("orders", req, #{
//!     sort:     ["id", "customer", "amount", "created"],   // за чим можна сортувати
//!     order:    "id desc",                                  // без ?sort= у адресі
//!     filters:  #{ status: "eq", customer: "contains", amount: "between" },
//!     per_page: 20,
//! });
//! ```
//!
//! ```html
//! <th><a href={g.sort_url.amount}>Сума {{ g.arrow.amount }}</a></th>
//! <tr @for={o in g.rows}>…</tr>
//! <a @for={n in g.page.window} href={g.page_url[`${n}`]}>{{ n }}</a>
//! ```
//!
//! Таблиця зі списком, сортуванням, фільтрами й сторінками — найчастіша
//! сторінка будь-якої адмінки, і щоразу та сама купа коду: прочитати
//! параметри з адреси, звірити колонку сортування зі списком дозволених (бо
//! інакше це ін'єкція в `order by`), зібрати фільтр, порахувати сторінки,
//! побудувати посилання, які зберігають усе інше. Тут це все — одна функція,
//! а розмітка лишається вашою.

use std::collections::BTreeMap;

use rhai::{Array, Dynamic, Engine, EvalAltResult, Map};
use rhaix_db::Database;

use crate::paginate::paginate;
use crate::stdlib::url_with_params;
use crate::web::Request;

type Error = Box<EvalAltResult>;

/// Оператори фільтра, які розуміє `db.find`.
const OPERATORS: [&str; 10] = [
    "eq", "ne", "contains", "starts", "ends", "gt", "gte", "lt", "lte", "between",
];

fn strings(value: &Dynamic, what: &str) -> Result<Vec<String>, Error> {
    let Some(array) = value.clone().try_cast::<Array>() else {
        return Err(format!("db.grid(): `{what}` має бути масивом рядків").into());
    };
    Ok(array.iter().map(crate::display).collect())
}

/// Значення з адреси: число — числом, інакше рядком. У Postgres порівняння
/// колонки-числа з рядком — помилка, а `?amount_from=100` приходить рядком.
fn typed(text: &str) -> Dynamic {
    if let Ok(number) = text.parse::<i64>() {
        return Dynamic::from(number);
    }
    if let Ok(number) = text.replace(',', ".").parse::<f64>() {
        return Dynamic::from(number);
    }
    Dynamic::from(text.to_owned())
}

pub fn grid(db: &Database, req: &Request, table: &str, options: &Map) -> Result<Map, Error> {
    let query = &req.data().query;
    let path = req.data().path.clone();

    let mut sortable: Vec<String> = Vec::new();
    let mut default = String::new();
    let mut filters: BTreeMap<String, String> = BTreeMap::new();
    let mut per_page: i64 = 20;
    let mut fixed = Map::new();
    let mut fields: Option<Dynamic> = None;
    for (key, value) in options {
        match key.as_str() {
            "sort" => sortable = strings(value, "sort")?,
            // Не `default`: у Rhai це зарезервоване слово.
            "order" => default = crate::display(value),
            "filters" => {
                let Some(map) = value.clone().try_cast::<Map>() else {
                    return Err("db.grid(): `filters` — мапа поле → оператор".into());
                };
                for (field, op) in map {
                    let op = crate::display(&op);
                    if !OPERATORS.contains(&op.as_str()) {
                        return Err(format!(
                            "db.grid(): невідомий оператор `{op}` для `{field}` (є {})",
                            OPERATORS.join(", ")
                        )
                        .into());
                    }
                    filters.insert(field.to_string(), op);
                }
            }
            "per_page" => {
                per_page = value
                    .as_int()
                    .map_err(|_| -> Error { "db.grid(): `per_page` — ціле число".into() })?
                    .clamp(1, 500)
            }
            // Фільтр, якого користувач не бачить і змінити не може:
            // `where: #{ owner_id: page.user.id }`.
            "where" => {
                fixed = value
                    .clone()
                    .try_cast::<Map>()
                    .ok_or_else(|| -> Error { "db.grid(): `where` — мапа".into() })?
            }
            "fields" => fields = Some(value.clone()),
            other => {
                return Err(format!(
                    "db.grid(): невідомий параметр `{other}` (є sort, order, filters, per_page, where, fields)"
                )
                .into())
            }
        }
    }

    // --------------------------------------------------------- сортування
    // Колонка з адреси потрапляє в `order by`, тож лише зі списку дозволених.
    let (default_column, default_dir) = match default.split_once(' ') {
        Some((column, dir)) => (column.trim().to_owned(), dir.trim().to_ascii_lowercase()),
        None if !default.is_empty() => (default.trim().to_owned(), "asc".to_owned()),
        None => (
            sortable.first().cloned().unwrap_or_else(|| "id".to_owned()),
            "desc".to_owned(),
        ),
    };
    let asked = query.get("sort").map(|s| s.trim().to_owned());
    let sort = match asked {
        Some(column) if sortable.contains(&column) => column,
        _ => default_column.clone(),
    };
    let dir = match query.get("dir").map(|d| d.trim().to_ascii_lowercase()) {
        Some(dir) if dir == "asc" || dir == "desc" => dir,
        _ if sort == default_column => default_dir.clone(),
        _ => "asc".to_owned(),
    };
    if dir != "asc" && dir != "desc" {
        return Err(format!("db.grid(): `order` — «колонка asc|desc», а не `{default}`").into());
    }

    // ----------------------------------------------------------- фільтри
    let mut filter = Map::new();
    let mut values = Map::new();
    // Стан адреси без сторінки: з нього будуються всі посилання, щоб
    // сортування не губило фільтр, а фільтр — сортування.
    let mut state: BTreeMap<String, String> = BTreeMap::new();
    for (field, op) in &filters {
        if op == "between" {
            let from_key = format!("{field}_from");
            let to_key = format!("{field}_to");
            let from = query.get(&from_key).map(|v| v.trim()).unwrap_or("");
            let to = query.get(&to_key).map(|v| v.trim()).unwrap_or("");
            let mut range = Map::new();
            if !from.is_empty() {
                range.insert("gte".into(), typed(from));
                state.insert(from_key.clone(), from.to_owned());
            }
            if !to.is_empty() {
                range.insert("lte".into(), typed(to));
                state.insert(to_key.clone(), to.to_owned());
            }
            values.insert(from_key.into(), Dynamic::from(from.to_owned()));
            values.insert(to_key.into(), Dynamic::from(to.to_owned()));
            if !range.is_empty() {
                filter.insert(field.as_str().into(), Dynamic::from_map(range));
            }
            continue;
        }
        let value = query.get(field).map(|v| v.trim()).unwrap_or("");
        values.insert(field.as_str().into(), Dynamic::from(value.to_owned()));
        if value.is_empty() {
            continue;
        }
        state.insert(field.clone(), value.to_owned());
        let typed_value = match op.as_str() {
            "contains" | "starts" | "ends" => Dynamic::from(value.to_owned()),
            _ => typed(value),
        };
        let condition = if op == "eq" {
            typed_value
        } else {
            let mut map = Map::new();
            map.insert(op.as_str().into(), typed_value);
            Dynamic::from_map(map)
        };
        filter.insert(field.as_str().into(), condition);
    }

    // `where` — межа, а не підказка: накладається останнім і перемагає.
    // Інакше `?owner_id=7` з адреси перезаписав би `where: #{ owner_id: me }`.
    for (field, condition) in fixed {
        filter.insert(field, condition);
    }

    // -------------------------------------------------------- сторінки
    let total = db.driver().count(table, &filter).map_err(fail)?;
    let asked_page = query
        .get("page")
        .and_then(|p| p.trim().parse::<i64>().ok())
        .unwrap_or(1);
    let page = paginate(total, per_page, asked_page);
    let skip = page["skip"].as_int().unwrap_or(0);

    let mut find_options = Map::new();
    find_options.insert("sort".into(), Dynamic::from(format!("{sort} {dir}")));
    find_options.insert("limit".into(), Dynamic::from(per_page));
    find_options.insert("skip".into(), Dynamic::from(skip));
    if let Some(fields) = fields {
        find_options.insert("fields".into(), fields);
    }
    let rows: Array = db
        .driver()
        .find(table, &filter, &find_options)
        .map_err(fail)?
        .into_iter()
        .map(Dynamic::from_map)
        .collect();

    // --------------------------------------------------------- посилання
    let link = |extra: &[(&str, String)]| -> String {
        let mut params = Map::new();
        for (key, value) in &state {
            params.insert(key.as_str().into(), Dynamic::from(value.clone()));
        }
        params.insert("sort".into(), Dynamic::from(sort.clone()));
        params.insert("dir".into(), Dynamic::from(dir.clone()));
        for (key, value) in extra {
            params.insert((*key).into(), Dynamic::from(value.clone()));
        }
        url_with_params(&path, params)
    };

    // Клац по поточній колонці перевертає напрям, по іншій — з `asc`.
    let mut sort_url = Map::new();
    let mut arrow = Map::new();
    for column in &sortable {
        let next_dir = if *column == sort && dir == "asc" {
            "desc"
        } else {
            "asc"
        };
        let mut params = Map::new();
        for (key, value) in &state {
            params.insert(key.as_str().into(), Dynamic::from(value.clone()));
        }
        params.insert("sort".into(), Dynamic::from(column.clone()));
        params.insert("dir".into(), Dynamic::from(next_dir.to_owned()));
        sort_url.insert(
            column.as_str().into(),
            Dynamic::from(url_with_params(&path, params)),
        );
        let mark = match (*column == sort, dir.as_str()) {
            (true, "asc") => "▲",
            (true, _) => "▼",
            _ => "",
        };
        arrow.insert(column.as_str().into(), Dynamic::from(mark.to_owned()));
    }

    // Посилання на кожну сторінку вікна плюс сусідні: ключ — номер рядком,
    // бо ключі мап у Rhai — рядки (`g.page_url[`${n}`]`).
    let mut page_url = Map::new();
    let pages = page["pages"].as_int().unwrap_or(1);
    let mut numbers: Vec<i64> = page["window"]
        .clone()
        .try_cast::<Array>()
        .unwrap_or_default()
        .iter()
        .filter_map(|n| n.as_int().ok())
        .collect();
    numbers.extend([
        1,
        pages,
        page["prev"].as_int().unwrap_or(1),
        page["next"].as_int().unwrap_or(1),
    ]);
    for n in numbers {
        page_url.insert(
            n.to_string().into(),
            Dynamic::from(link(&[("page", n.to_string())])),
        );
    }

    let mut out = Map::new();
    out.insert("rows".into(), Dynamic::from_array(rows));
    out.insert("page".into(), Dynamic::from_map(page.clone()));
    out.insert("total".into(), Dynamic::from(total));
    out.insert("sort".into(), Dynamic::from(sort.clone()));
    out.insert("dir".into(), Dynamic::from(dir.clone()));
    out.insert("values".into(), Dynamic::from_map(values));
    out.insert("sort_url".into(), Dynamic::from_map(sort_url));
    out.insert("arrow".into(), Dynamic::from_map(arrow));
    out.insert("page_url".into(), Dynamic::from_map(page_url));
    out.insert(
        "prev_url".into(),
        Dynamic::from(link(&[("page", page["prev"].to_string())])),
    );
    out.insert(
        "next_url".into(),
        Dynamic::from(link(&[("page", page["next"].to_string())])),
    );
    // Скинути фільтри, але лишити сортування.
    let mut reset = Map::new();
    reset.insert("sort".into(), Dynamic::from(sort));
    reset.insert("dir".into(), Dynamic::from(dir));
    out.insert(
        "reset_url".into(),
        Dynamic::from(url_with_params(&path, reset)),
    );
    out.insert("filtered".into(), Dynamic::from(!state.is_empty()));
    Ok(out)
}

fn fail(err: rhaix_db::DbError) -> Error {
    err.to_string().into()
}

pub fn register_grid(engine: &mut Engine) {
    engine
        .register_fn(
            "grid",
            |db: &mut Database, table: &str, req: Request| -> Result<Map, Error> {
                grid(db, &req, table, &Map::new())
            },
        )
        .register_fn(
            "grid",
            |db: &mut Database, table: &str, req: Request, options: Map| -> Result<Map, Error> {
                grid(db, &req, table, &options)
            },
        );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::{parse_urlencoded, RequestData};
    use crate::{engine, Limits};
    use rhai::Scope;

    fn database() -> Database {
        let db = Database::open("sqlite", ":memory:").expect("база");
        let driver = db.driver();
        driver
            .raw_exec(
                "create table orders (id integer primary key, customer text, amount real, status text)",
                &[],
            )
            .unwrap();
        for i in 1..=45 {
            let status = if i % 3 == 0 { "paid" } else { "new" };
            driver
                .raw_exec(
                    "insert into orders (customer, amount, status) values (?, ?, ?)",
                    &[
                        Dynamic::from(format!("Клієнт {i}")),
                        Dynamic::from(i as f64 * 10.0),
                        Dynamic::from(status.to_owned()),
                    ],
                )
                .unwrap();
        }
        db
    }

    fn run(url: &str, script: &str) -> Dynamic {
        let (path, query) = url.split_once('?').unwrap_or((url, ""));
        let request = RequestData {
            method: "GET".into(),
            path: path.into(),
            url: url.into(),
            query: parse_urlencoded(query),
            ..Default::default()
        };
        let engine = engine(Limits::default());
        let mut scope = Scope::new();
        scope.push("db", database());
        scope.push("req", Request::new(request));
        engine
            .eval_with_scope::<Dynamic>(&mut scope, script)
            .unwrap_or_else(|err| panic!("{err}"))
    }

    const GRID: &str = r#"db.grid("orders", req, #{
        sort: ["id", "amount", "customer"],
        order: "id desc",
        filters: #{ status: "eq", customer: "contains", amount: "between" },
        per_page: 10,
    })"#;

    fn get(map: &Map, key: &str) -> Dynamic {
        map.get(key).cloned().unwrap_or(Dynamic::UNIT)
    }

    #[test]
    fn default_sort_paging_and_links() {
        let g = run("/orders", GRID).cast::<Map>();
        let rows = get(&g, "rows").cast::<Array>();
        assert_eq!(rows.len(), 10);
        assert_eq!(
            rows[0].clone().cast::<Map>()["id"].as_int().unwrap(),
            45,
            "id desc"
        );
        assert_eq!(get(&g, "total").as_int().unwrap(), 45);
        assert_eq!(get(&g, "page").cast::<Map>()["pages"].as_int().unwrap(), 5);

        let sort_url = get(&g, "sort_url").cast::<Map>();
        assert_eq!(
            sort_url["amount"].to_string(),
            "/orders?dir=asc&sort=amount"
        );
        // Поточна колонка при desc — наступний клац дає asc.
        assert_eq!(sort_url["id"].to_string(), "/orders?dir=asc&sort=id");
        assert_eq!(get(&g, "arrow").cast::<Map>()["id"].to_string(), "▼");
        let page_url = get(&g, "page_url").cast::<Map>();
        assert_eq!(page_url["2"].to_string(), "/orders?dir=desc&page=2&sort=id");
    }

    #[test]
    fn filters_sort_and_page_travel_together_in_links() {
        let g = run(
            "/orders?status=paid&amount_from=100&sort=amount&dir=asc&page=2&customer=",
            GRID,
        )
        .cast::<Map>();
        // paid — кожне третє: 3,6,…,45 → 15 записів; з amount ≥ 100 (id ≥ 10) → 12.
        assert_eq!(get(&g, "total").as_int().unwrap(), 12);
        let rows = get(&g, "rows").cast::<Array>();
        assert_eq!(rows.len(), 2, "друга сторінка з 12 по 10");
        assert_eq!(rows[0].clone().cast::<Map>()["id"].as_int().unwrap(), 42);

        let sort_url = get(&g, "sort_url").cast::<Map>();
        assert_eq!(
            sort_url["customer"].to_string(),
            "/orders?amount_from=100&dir=asc&sort=customer&status=paid",
            "фільтр живе в посиланні сортування, сторінка — ні"
        );
        assert_eq!(
            get(&g, "values").cast::<Map>()["status"].to_string(),
            "paid"
        );
        assert!(get(&g, "filtered").as_bool().unwrap());
        assert_eq!(
            get(&g, "reset_url").to_string(),
            "/orders?dir=asc&sort=amount"
        );
    }

    #[test]
    fn a_sort_column_from_the_url_must_be_on_the_list() {
        // `order by` із адреси — класична ін'єкція; чужа колонка просто ігнорується.
        let g = run("/orders?sort=id;drop%20table%20orders&dir=sideways", GRID).cast::<Map>();
        assert_eq!(get(&g, "sort").to_string(), "id");
        assert_eq!(get(&g, "dir").to_string(), "desc");
        assert_eq!(get(&g, "total").as_int().unwrap(), 45);
    }

    #[test]
    fn a_hidden_where_cannot_be_overridden_from_the_url() {
        let g = run(
            "/orders?status=new",
            r#"db.grid("orders", req, #{ filters: #{ status: "eq" }, where: #{ status: "paid" } })"#,
        )
        .cast::<Map>();
        // `?status=new` не може перебити `where: status = paid`.
        assert_eq!(get(&g, "total").as_int().unwrap(), 15);
        let rows = get(&g, "rows").cast::<Array>();
        assert!(rows
            .iter()
            .all(|r| r.clone().cast::<Map>()["status"].to_string() == "paid"));
    }

    #[test]
    fn typos_in_options_are_errors() {
        let engine = engine(Limits::default());
        let mut scope = Scope::new();
        scope.push("db", database());
        scope.push("req", Request::new(RequestData::default()));
        let err = engine
            .eval_with_scope::<Dynamic>(&mut scope, r#"db.grid("orders", req, #{ filter: #{} })"#)
            .unwrap_err();
        assert!(err.to_string().contains("невідомий параметр"), "{err}");
        let err = engine
            .eval_with_scope::<Dynamic>(
                &mut scope,
                r#"db.grid("orders", req, #{ filters: #{ status: "like" } })"#,
            )
            .unwrap_err();
        assert!(err.to_string().contains("невідомий оператор"), "{err}");
    }
}
