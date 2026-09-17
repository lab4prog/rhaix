//! `db` у скрипті.
//!
//! Два рівні зі специфікації (PLAN 5.1): рідні запити (`db.query`) і
//! переносимий CRUD (`db.find/get/insert/update/delete/count`). Помилки
//! віддаються як звичайні помилки Rhai, тому потрапляють у діагностику з
//! позицією в `.rhx` — так само, як будь-яка інша помилка виразу.

use rhai::{Array, Dynamic, Engine, EvalAltResult, FnPtr, Map, NativeCallContext};
use rhaix_db::{Database, DbError};

/// Зареєструвати тип `db`.
pub fn register_db(engine: &mut Engine) {
    engine
        .register_type_with_name::<Database>("Database")
        .register_get("kind", |db: &mut Database| db.kind().to_string())
        // ---------------------------------------------- рідні запити
        .register_fn("query", |db: &mut Database, sql: &str| {
            rows(db.driver().raw_query(sql, &[]))
        })
        .register_fn("query", |db: &mut Database, sql: &str, params: Array| {
            rows(db.driver().raw_query(sql, &params))
        })
        .register_fn("one", |db: &mut Database, sql: &str| {
            first(db.driver().raw_query(sql, &[]))
        })
        .register_fn("one", |db: &mut Database, sql: &str, params: Array| {
            first(db.driver().raw_query(sql, &params))
        })
        .register_fn("exec", |db: &mut Database, sql: &str| {
            affected(db.driver().raw_exec(sql, &[]))
        })
        .register_fn("exec", |db: &mut Database, sql: &str, params: Array| {
            affected(db.driver().raw_exec(sql, &params))
        })
        .register_fn("tx", transaction)
        // ------------------------------------------- переносимий CRUD
        .register_fn("find", |db: &mut Database, table: &str| {
            rows(db.driver().find(table, &Map::new(), &Map::new()))
        })
        .register_fn("find", |db: &mut Database, table: &str, filter: Map| {
            rows(db.driver().find(table, &filter, &Map::new()))
        })
        .register_fn(
            "find",
            |db: &mut Database, table: &str, filter: Map, options: Map| {
                rows(db.driver().find(table, &filter, &options))
            },
        )
        .register_fn("one", |db: &mut Database, table: &str, filter: Map| {
            first(db.driver().find(table, &filter, &Map::new()))
        })
        .register_fn("get", |db: &mut Database, table: &str, id: Dynamic| {
            match db.driver().get(table, id) {
                Ok(Some(row)) => Ok(Dynamic::from_map(row)),
                // Запису немає — це `()`, щоб працювало звичне `?? default`
                Ok(None) => Ok(Dynamic::UNIT),
                Err(err) => Err(fail(err)),
            }
        })
        .register_fn("count", |db: &mut Database, table: &str| {
            db.driver().count(table, &Map::new()).map_err(fail)
        })
        .register_fn("count", |db: &mut Database, table: &str, filter: Map| {
            db.driver().count(table, &filter).map_err(fail)
        })
        .register_fn("insert", |db: &mut Database, table: &str, values: Map| {
            // Повертається ідентифікатор нового запису — його майже завжди
            // треба одразу далі, у посиланні чи наступному запиті.
            db.driver()
                .insert(table, &values)
                .map(|done| done.last_id)
                .map_err(fail)
        })
        .register_fn(
            "update",
            |db: &mut Database, table: &str, id: Dynamic, values: Map| {
                db.driver()
                    .update(table, id, &values)
                    .map(|done| done.rows)
                    .map_err(fail)
            },
        )
        .register_fn("delete", |db: &mut Database, table: &str, id: Dynamic| {
            db.driver()
                .delete(table, id)
                .map(|done| done.rows)
                .map_err(fail)
        });
}

fn rows(result: Result<Vec<Map>, DbError>) -> Result<Array, Box<EvalAltResult>> {
    result
        .map(|rows| rows.into_iter().map(Dynamic::from_map).collect())
        .map_err(fail)
}

fn first(result: Result<Vec<Map>, DbError>) -> Result<Dynamic, Box<EvalAltResult>> {
    result
        .map(|rows| {
            rows.into_iter()
                .next()
                .map(Dynamic::from_map)
                .unwrap_or(Dynamic::UNIT)
        })
        .map_err(fail)
}

fn affected(result: Result<rhaix_db::Affected, DbError>) -> Result<i64, Box<EvalAltResult>> {
    result.map(|done| done.rows).map_err(fail)
}

fn fail(err: DbError) -> Box<EvalAltResult> {
    err.to_string().into()
}

/// `db.tx(|t| { ... })` — кілька запитів однією транзакцією.
///
/// `t` — та сама база, але прив'язана до одного з'єднання. Це принципово:
/// `db.exec("begin")` узяв би з пулу одне з'єднання, а наступний `insert` —
/// інше, і «транзакція» не охопила б нічого.
///
/// Будь-яка помилка всередині (і помилка бази, і `throw` у скрипті) означає
/// rollback: сторінка не має лишати базу в напівзміненому стані.
fn transaction(
    context: NativeCallContext,
    db: &mut Database,
    body: FnPtr,
) -> Result<Dynamic, Box<EvalAltResult>> {
    // Помилку скрипта треба винести назовні як є — із позицією у файлі.
    // Трейт бази про Rhai не знає, тому вона їде в цій комірці, а в трейт
    // повертається звичайна помилка, якої достатньо для rollback.
    let mut script_error: Option<Box<EvalAltResult>> = None;

    let outcome = db.transaction(&mut |pinned| {
        let handle = Dynamic::from(Database::new(pinned));
        match body.call_within_context::<Dynamic>(&context, (handle,)) {
            Ok(value) => Ok(value),
            Err(err) => {
                script_error = Some(err);
                Err(DbError::Query("транзакцію перервано".into()))
            }
        }
    });

    if let Some(err) = script_error {
        return Err(err);
    }
    outcome.map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{engine, Limits};
    use rhai::Scope;

    fn database() -> Database {
        let db = Database::open("sqlite", ":memory:").expect("база в пам'яті");
        db.driver()
            .raw_exec(
                "create table todos (id integer primary key, title text not null, done integer not null default 0)",
                &[],
            )
            .expect("таблиця");
        db
    }

    fn eval(script: &str) -> Result<Dynamic, String> {
        let engine = engine(Limits::default());
        let mut scope = Scope::new();
        scope.push("db", database());
        engine
            .eval_with_scope::<Dynamic>(&mut scope, script)
            .map_err(|err| err.to_string())
    }

    #[test]
    fn crud_from_a_script() {
        let value = eval(
            r#"
            let id = db.insert("todos", #{ title: "молоко", done: false });
            db.insert("todos", #{ title: "хліб", done: true });
            let all = db.find("todos", #{}, #{ sort: "id asc" });
            let one = db.get("todos", id);
            db.update("todos", id, #{ done: true });
            [all.len(), one.title, db.count("todos", #{ done: true })]
            "#,
        )
        .expect("скрипт");
        let array = value.cast::<Array>();
        assert_eq!(array[0].as_int().unwrap(), 2);
        assert_eq!(array[1].to_string(), "молоко");
        assert_eq!(array[2].as_int().unwrap(), 2);
    }

    #[test]
    fn missing_record_is_unit() {
        let value = eval(r#"db.get("todos", 999) ?? "немає""#).expect("скрипт");
        assert_eq!(value.to_string(), "немає");
    }

    #[test]
    fn raw_query_takes_parameters() {
        let value = eval(
            r#"
            db.insert("todos", #{ title: "молоко", done: 0 });
            db.query("select title from todos where title = ?", ["молоко"]).len()
            "#,
        )
        .expect("скрипт");
        assert_eq!(value.as_int().unwrap(), 1);
    }

    #[test]
    fn database_errors_reach_the_script_in_ukrainian() {
        let err = eval(r#"db.find("no_such_table")"#).unwrap_err();
        assert!(err.contains("запит до бази"), "{err}");
        assert!(err.contains("no_such_table"), "{err}");
    }
}
