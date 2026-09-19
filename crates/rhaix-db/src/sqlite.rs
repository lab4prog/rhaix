//! Драйвер SQLite.
//!
//! З'єднання беруться з маленького пулу: рендер іде в `spawn_blocking`, тож
//! паралельних запитів рівно стільки, скільки потоків, і одне з'єднання під
//! м'ютексом швидко стало б вузьким місцем.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rhai::{Dynamic, Map};
use rusqlite::types::{ToSqlOutput, Value, ValueRef};
use rusqlite::{Connection, ToSql};

use crate::{Affected, DbDriver, DbError};

/// Скільки з'єднань тримати. Більше немає сенсу: SQLite і так серіалізує запис.
const POOL_SIZE: usize = 4;

pub struct SqliteDriver {
    path: PathBuf,
    pool: Mutex<Vec<Connection>>,
}

impl SqliteDriver {
    /// `url` — шлях до файлу або `:memory:`.
    pub fn open(url: &str) -> Result<Arc<Self>, DbError> {
        let path = PathBuf::from(url);
        if url != ":memory:" {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).map_err(|err| {
                        DbError::Config(format!("не вдалося створити теку для бази: {err}"))
                    })?;
                }
            }
        }

        let driver = Arc::new(Self {
            path,
            pool: Mutex::new(Vec::new()),
        });
        // Перевіряємо одразу: краще впасти на старті, ніж на першому запиті.
        let connection = driver.checkout()?;
        driver.checkin(connection);
        Ok(driver)
    }

    fn open_connection(&self) -> Result<Connection, DbError> {
        let connection = if self.path == Path::new(":memory:") {
            Connection::open_in_memory()
        } else {
            Connection::open(&self.path)
        }
        .map_err(|err| DbError::Config(format!("не вдалося відкрити базу: {err}")))?;

        // WAL — щоб читання не блокувалося записом; foreign_keys — бо інакше
        // SQLite мовчки ігнорує зовнішні ключі.
        let _ = connection.pragma_update(None, "journal_mode", "WAL");
        let _ = connection.pragma_update(None, "foreign_keys", "ON");
        let _ = connection.busy_timeout(std::time::Duration::from_secs(5));
        Ok(connection)
    }

    fn checkout(&self) -> Result<Connection, DbError> {
        if let Some(connection) = self.pool.lock().expect("пул не отруєний").pop() {
            return Ok(connection);
        }
        self.open_connection()
    }

    fn checkin(&self, connection: Connection) {
        let mut pool = self.pool.lock().expect("пул не отруєний");
        if pool.len() < POOL_SIZE {
            pool.push(connection);
        }
    }

    fn with_connection<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, DbError>,
    ) -> Result<T, DbError> {
        let connection = self.checkout()?;
        let result = f(&connection);
        self.checkin(connection);
        result
    }
}

impl DbDriver for SqliteDriver {
    fn kind(&self) -> &'static str {
        "sqlite"
    }

    fn raw_query(&self, sql: &str, params: &[Dynamic]) -> Result<Vec<Map>, DbError> {
        self.with_connection(|connection| run_query(connection, sql, params))
    }

    fn raw_exec(&self, sql: &str, params: &[Dynamic]) -> Result<Affected, DbError> {
        self.with_connection(|connection| run_exec(connection, sql, params))
    }

    /// Транзакція тримає одне з'єднання від `begin` до `commit`.
    fn transaction(
        &self,
        body: &mut dyn FnMut(Arc<dyn DbDriver>) -> Result<Dynamic, DbError>,
    ) -> Result<Dynamic, DbError> {
        let connection = self.checkout()?;
        // `begin immediate` — щоб конфлікт запису виявився одразу, а не на
        // `commit`, коли відкочувати вже дорожче.
        connection
            .execute_batch("begin immediate;")
            .map_err(|err| DbError::Query(format!("не вдалося почати транзакцію: {err}")))?;

        let pinned = Arc::new(PinnedSqlite {
            connection: Mutex::new(Some(connection)),
        });
        let result = body(pinned.clone());

        // З'єднання забираємо назад. Якщо скрипт десь зберіг посилання на
        // транзакцію, `take` віддасть None — тоді просто нічого не робимо,
        // і з'єднання помре разом із останнім посиланням.
        let Some(connection) = pinned.take() else {
            return Err(DbError::Query(
                "з'єднання транзакції лишилось зайнятим: не зберігайте `t` поза межами db.tx"
                    .into(),
            ));
        };

        match &result {
            Ok(_) => connection
                .execute_batch("commit;")
                .map_err(|err| DbError::Query(format!("не вдалося завершити транзакцію: {err}")))?,
            Err(_) => {
                let _ = connection.execute_batch("rollback;");
            }
        }
        self.checkin(connection);
        result
    }

    fn migrate(&self, migrations: &[(String, String)]) -> Result<Vec<String>, DbError> {
        self.with_connection(|connection| {
            connection
                .execute_batch(
                    "create table if not exists _rhaix_migrations (
                         name text primary key,
                         applied_at text not null default (datetime('now'))
                     )",
                )
                .map_err(|err| {
                    DbError::Query(format!("не вдалося створити таблицю міграцій: {err}"))
                })?;
            migrate_all(connection, migrations)
        })
    }
}

/// Драйвер, прив'язаний до одного з'єднання: усередині `db.tx`.
struct PinnedSqlite {
    connection: Mutex<Option<Connection>>,
}

impl PinnedSqlite {
    fn take(&self) -> Option<Connection> {
        self.connection.lock().expect("з'єднання не отруєне").take()
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> Result<T, DbError>) -> Result<T, DbError> {
        let guard = self.connection.lock().expect("з'єднання не отруєне");
        match guard.as_ref() {
            Some(connection) => f(connection),
            None => Err(DbError::Query("транзакцію вже завершено".into())),
        }
    }
}

impl DbDriver for PinnedSqlite {
    fn kind(&self) -> &'static str {
        "sqlite"
    }

    fn raw_query(&self, sql: &str, params: &[Dynamic]) -> Result<Vec<Map>, DbError> {
        self.with(|connection| run_query(connection, sql, params))
    }

    fn raw_exec(&self, sql: &str, params: &[Dynamic]) -> Result<Affected, DbError> {
        self.with(|connection| run_exec(connection, sql, params))
    }
}

fn run_query(connection: &Connection, sql: &str, params: &[Dynamic]) -> Result<Vec<Map>, DbError> {
    let mut statement = connection
        .prepare(sql)
        .map_err(|err| query_error(sql, err))?;
    let columns: Vec<String> = statement
        .column_names()
        .into_iter()
        .map(|name| name.to_owned())
        .collect();

    let bound: Vec<Param> = params.iter().map(Param).collect();
    let values: Vec<&dyn ToSql> = bound.iter().map(|p| p as &dyn ToSql).collect();

    let mut rows = statement
        .query(values.as_slice())
        .map_err(|err| query_error(sql, err))?;

    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(|err| query_error(sql, err))? {
        let mut map = Map::new();
        for (index, name) in columns.iter().enumerate() {
            let value = row
                .get_ref(index)
                .map_err(|err| query_error(sql, err))
                .map(from_sql)?;
            map.insert(name.as_str().into(), value);
        }
        out.push(map);
    }
    Ok(out)
}

fn run_exec(connection: &Connection, sql: &str, params: &[Dynamic]) -> Result<Affected, DbError> {
    let bound: Vec<Param> = params.iter().map(Param).collect();
    let values: Vec<&dyn ToSql> = bound.iter().map(|p| p as &dyn ToSql).collect();
    let rows = connection
        .execute(sql, values.as_slice())
        .map_err(|err| query_error(sql, err))?;
    Ok(Affected {
        rows: rows as i64,
        last_id: connection.last_insert_rowid(),
    })
}

fn migrate_all(
    connection: &Connection,
    migrations: &[(String, String)],
) -> Result<Vec<String>, DbError> {
    let mut applied = Vec::new();
    for (name, body) in migrations {
        let already: i64 = connection
            .query_row(
                "select count(*) from _rhaix_migrations where name = ?",
                [name],
                |row| row.get(0),
            )
            .map_err(|err| DbError::Query(err.to_string()))?;
        if already > 0 {
            continue;
        }

        // Міграція йде однією транзакцією: або вся, або жодної.
        connection
            .execute_batch(&format!("begin; {body}; commit;"))
            .map_err(|err| {
                let _ = connection.execute_batch("rollback;");
                DbError::Query(format!("міграція `{name}`: {err}"))
            })?;
        connection
            .execute("insert into _rhaix_migrations (name) values (?)", [name])
            .map_err(|err| DbError::Query(err.to_string()))?;
        applied.push(name.clone());
    }
    Ok(applied)
}

/// Обгортка, щоб `Dynamic` можна було віддати в rusqlite як параметр.
struct Param<'a>(&'a Dynamic);

impl ToSql for Param<'_> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        let value = self.0;
        if value.is_unit() {
            return Ok(ToSqlOutput::Owned(Value::Null));
        }
        if let Ok(flag) = value.as_bool() {
            return Ok(ToSqlOutput::Owned(Value::Integer(i64::from(flag))));
        }
        if let Ok(number) = value.as_int() {
            return Ok(ToSqlOutput::Owned(Value::Integer(number)));
        }
        if let Ok(number) = value.as_float() {
            return Ok(ToSqlOutput::Owned(Value::Real(number)));
        }
        if let Some(text) = value.read_lock::<rhai::ImmutableString>() {
            return Ok(ToSqlOutput::Owned(Value::Text(text.to_string())));
        }
        if let Some(blob) = value.read_lock::<rhai::Blob>() {
            return Ok(ToSqlOutput::Owned(Value::Blob(blob.clone())));
        }
        // Мапи й масиви в SQLite не мають типу — віддаємо як текст, щоб людина
        // побачила, що саме поїхало, а не отримала мовчазний NULL.
        Ok(ToSqlOutput::Owned(Value::Text(value.to_string())))
    }
}

fn from_sql(value: ValueRef<'_>) -> Dynamic {
    match value {
        ValueRef::Null => Dynamic::UNIT,
        ValueRef::Integer(number) => Dynamic::from(number),
        ValueRef::Real(number) => Dynamic::from(number),
        ValueRef::Text(bytes) => Dynamic::from(String::from_utf8_lossy(bytes).into_owned()),
        ValueRef::Blob(bytes) => Dynamic::from_blob(bytes.to_vec()),
    }
}

/// Помилки SQLite англійські й без контексту — додаємо сам запит.
fn query_error(sql: &str, err: rusqlite::Error) -> DbError {
    DbError::Query(format!("{err}\n  запит: {}", sql.trim()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_transaction_commits_together() {
        let driver = SqliteDriver::open(":memory:").expect("база");
        driver
            .raw_exec("create table t (id integer primary key, n integer)", &[])
            .expect("таблиця");

        let result = driver.transaction(&mut |tx| {
            tx.raw_exec("insert into t (n) values (1)", &[])?;
            tx.raw_exec("insert into t (n) values (2)", &[])?;
            Ok(Dynamic::from(true))
        });

        assert!(result.is_ok());
        assert_eq!(driver.raw_query("select * from t", &[]).unwrap().len(), 2);
    }

    #[test]
    fn a_failed_transaction_leaves_nothing_behind() {
        let driver = SqliteDriver::open(":memory:").expect("база");
        driver
            .raw_exec("create table t (id integer primary key, n integer)", &[])
            .expect("таблиця");
        driver
            .raw_exec("insert into t (n) values (1)", &[])
            .expect("перший запис");

        let result = driver.transaction(&mut |tx| {
            tx.raw_exec("insert into t (n) values (2)", &[])?;
            // Другий запит падає — перший не має лишитись.
            tx.raw_exec("insert into nosuchtable (n) values (3)", &[])?;
            Ok(Dynamic::UNIT)
        });

        assert!(result.is_err());
        let rows = driver.raw_query("select * from t", &[]).unwrap();
        assert_eq!(rows.len(), 1, "відкат мав прибрати запис із транзакції");
    }

    #[test]
    fn the_connection_returns_to_the_pool_after_a_transaction() {
        // Інакше кожна транзакція «з'їдала» б з'єднання, і застосунок
        // помирав би після кількох записів.
        let driver = SqliteDriver::open(":memory:").expect("база");
        driver
            .raw_exec("create table t (id integer primary key)", &[])
            .expect("таблиця");
        for _ in 0..10 {
            let _ = driver
                .transaction(&mut |tx| {
                    tx.raw_exec("insert into t default values", &[])?;
                    Ok(Dynamic::UNIT)
                })
                .expect("транзакція");
        }
        assert_eq!(driver.raw_query("select * from t", &[]).unwrap().len(), 10);
    }

    use super::*;

    fn driver() -> Arc<SqliteDriver> {
        let driver = SqliteDriver::open(":memory:").expect("база в пам'яті");
        driver
            .raw_exec(
                "create table todos (id integer primary key, title text not null, done integer not null default 0)",
                &[],
            )
            .expect("таблиця");
        driver
    }

    #[test]
    fn insert_and_read_back() {
        let db = driver();
        let affected = db
            .insert(
                "todos",
                &crate::tests::map(&[("title", Dynamic::from("молоко"))]),
            )
            .expect("вставка");
        assert_eq!(affected.rows, 1);
        assert!(affected.last_id > 0);

        let rows = db.find("todos", &Map::new(), &Map::new()).expect("вибірка");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["title"].to_string(), "молоко");
        // булеве значення в SQLite — ціле, і truthiness rhaix це враховує
        assert_eq!(rows[0]["done"].as_int().unwrap(), 0);
    }

    #[test]
    fn null_becomes_unit() {
        let db = driver();
        db.raw_exec("alter table todos add column note text", &[])
            .expect("колонка");
        db.raw_exec(
            "insert into todos (title, done, note) values ('є', 0, null)",
            &[],
        )
        .expect("вставка");

        let rows = db
            .raw_query("select note from todos", &[])
            .expect("вибірка");
        // NULL має приходити як `()`, щоб у шаблоні працювало `?? "—"`
        assert!(rows[0]["note"].is_unit());
    }

    #[test]
    fn migrations_run_once() {
        let db = SqliteDriver::open(":memory:").unwrap();
        let migrations = vec![(
            "001_init.sql".to_owned(),
            "create table notes (id integer primary key, body text)".to_owned(),
        )];

        let applied = db.migrate(&migrations).expect("міграція");
        assert_eq!(applied, vec!["001_init.sql".to_owned()]);

        let applied = db.migrate(&migrations).expect("повторний запуск");
        assert!(applied.is_empty(), "друга спроба нічого не робить");
    }

    #[test]
    fn broken_migration_reports_its_name() {
        let db = SqliteDriver::open(":memory:").unwrap();
        let err = db
            .migrate(&[("002_bad.sql".to_owned(), "not sql at all".to_owned())])
            .unwrap_err();
        assert!(err.to_string().contains("002_bad.sql"), "{err}");
    }

    #[test]
    fn query_errors_include_the_statement() {
        let db = driver();
        let err = db
            .raw_query("select * from missing_table", &[])
            .unwrap_err();
        assert!(err.to_string().contains("missing_table"), "{err}");
        assert!(err.to_string().contains("запит:"), "{err}");
    }
}
