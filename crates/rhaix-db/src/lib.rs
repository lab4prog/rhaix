//! Шар даних rhaix: трейт драйвера, переносимий CRUD і драйвер SQLite.
//!
//! Два рівні (PLAN 5.1):
//!
//! - **рідні запити** — `db.query("select …", [params])`, повна сила конкретної БД;
//! - **переносимий CRUD** — `db.find/get/insert/update/delete/count`, однаковий
//!   на всіх драйверах.
//!
//! Переносимий рівень має **типову** реалізацію прямо в трейті: вона будує
//! параметризований SQL. Драйвер, для якого SQL не рідний (Mongo, Surreal),
//! перевизначить ці методи — саме заради цього вони й у трейті.

#[cfg(feature = "postgres")]
mod postgres;
mod query;
#[cfg(feature = "sqlite")]
mod sqlite;

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use rhai::{Dynamic, Map};

#[cfg(feature = "postgres")]
pub use postgres::{PostgresDriver, DEFAULT_POOL as POSTGRES_DEFAULT_POOL};
pub use query::{ident, Sql, PRIMARY_KEY};
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteDriver;

/// Помилка роботи з базою.
#[derive(Debug, Clone)]
pub enum DbError {
    /// Неправильне налаштування: не відкрився файл, невідомий драйвер.
    Config(String),
    /// Помилка самого запиту.
    Query(String),
    /// Драйвер не вміє того, що просять.
    Unsupported(String),
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::Config(message) => write!(f, "база даних: {message}"),
            DbError::Query(message) => write!(f, "запит до бази: {message}"),
            DbError::Unsupported(message) => write!(f, "драйвер не підтримує: {message}"),
        }
    }
}

impl std::error::Error for DbError {}

/// Скільки рядків зачепив запис і який ідентифікатор отримав новий запис.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Affected {
    pub rows: i64,
    pub last_id: i64,
}

/// Драйвер бази. Обов'язкові лише рідні запити — решта має типову реалізацію.
pub trait DbDriver: Send + Sync {
    fn kind(&self) -> &'static str;

    fn raw_query(&self, sql: &str, params: &[Dynamic]) -> Result<Vec<Map>, DbError>;
    fn raw_exec(&self, sql: &str, params: &[Dynamic]) -> Result<Affected, DbError>;

    /// Виконати кілька запитів однією транзакцією.
    ///
    /// `body` отримує драйвер, прив'язаний до **одного** з'єднання. Це не
    /// дрібниця: `db.exec("begin")` з пулу взяв би одне з'єднання, а наступний
    /// `insert` — інше, і «транзакція» нічого б не охопила.
    ///
    /// Помилка з `body` означає rollback. Успіх — commit.
    fn transaction(
        &self,
        _body: &mut dyn FnMut(Arc<dyn DbDriver>) -> Result<Dynamic, DbError>,
    ) -> Result<Dynamic, DbError> {
        Err(DbError::Unsupported("транзакції".into()))
    }

    /// Застосувати міграції, повернути імена щойно застосованих.
    fn migrate(&self, _migrations: &[(String, String)]) -> Result<Vec<String>, DbError> {
        Err(DbError::Unsupported("міграції".into()))
    }

    // ------------------------------------------------- переносимий рівень

    fn find(&self, table: &str, filter: &Map, options: &Map) -> Result<Vec<Map>, DbError> {
        let sql = query::find(table, filter, options)?;
        self.raw_query(&sql.text, &sql.params)
    }

    fn get(&self, table: &str, id: Dynamic) -> Result<Option<Map>, DbError> {
        let sql = query::get(table, id)?;
        Ok(self.raw_query(&sql.text, &sql.params)?.into_iter().next())
    }

    fn count(&self, table: &str, filter: &Map) -> Result<i64, DbError> {
        let sql = query::count(table, filter)?;
        let rows = self.raw_query(&sql.text, &sql.params)?;
        Ok(rows
            .first()
            .and_then(|row| row.get("n"))
            .and_then(|value| value.as_int().ok())
            .unwrap_or(0))
    }

    fn insert(&self, table: &str, values: &Map) -> Result<Affected, DbError> {
        let sql = query::insert(table, values)?;
        self.raw_exec(&sql.text, &sql.params)
    }

    fn update(&self, table: &str, id: Dynamic, values: &Map) -> Result<Affected, DbError> {
        let sql = query::update(table, id, values)?;
        self.raw_exec(&sql.text, &sql.params)
    }

    fn delete(&self, table: &str, id: Dynamic) -> Result<Affected, DbError> {
        let sql = query::delete(table, id)?;
        self.raw_exec(&sql.text, &sql.params)
    }
}

/// Заглушка, коли база не налаштована.
///
/// Без неї `db.find(...)` давав би «невідома змінна `db`» — технічно правду,
/// але не ту, яка допомагає. Так людина одразу читає, чого бракує.
struct MissingDriver;

impl DbDriver for MissingDriver {
    fn kind(&self) -> &'static str {
        "none"
    }

    fn raw_query(&self, _sql: &str, _params: &[Dynamic]) -> Result<Vec<Map>, DbError> {
        Err(Self::not_configured())
    }

    fn raw_exec(&self, _sql: &str, _params: &[Dynamic]) -> Result<Affected, DbError> {
        Err(Self::not_configured())
    }
}

impl MissingDriver {
    fn not_configured() -> DbError {
        DbError::Config(
            "не налаштована. Додайте в `rhaix.toml`:

  [db]
  driver = \"sqlite\"
  url = \"data/app.db\""
                .into(),
        )
    }
}

/// `db` у скрипті: тонка обгортка над драйвером.
#[derive(Clone)]
pub struct Database(Arc<dyn DbDriver>);

impl Database {
    pub fn new(driver: Arc<dyn DbDriver>) -> Self {
        Self(driver)
    }

    /// База, якої немає: будь-який виклик пояснить, що додати в конфіг.
    pub fn unconfigured() -> Self {
        Self::new(Arc::new(MissingDriver))
    }

    /// Відкрити базу за налаштуваннями з `rhaix.toml`.
    pub fn open(driver: &str, url: &str) -> Result<Self, DbError> {
        Self::open_with_pool(driver, url, None)
    }

    /// Те саме з `[db] pool` — скільки з'єднань тримати. Коли всі зайняті,
    /// запит чекає вільного, а не відкриває нове.
    #[allow(unused_variables)]
    pub fn open_with_pool(driver: &str, url: &str, pool: Option<usize>) -> Result<Self, DbError> {
        match driver {
            #[cfg(feature = "sqlite")]
            "sqlite" => Ok(Self::new(SqliteDriver::open_with_pool(
                url,
                pool.unwrap_or(sqlite::DEFAULT_POOL),
            )?)),
            #[cfg(feature = "postgres")]
            "postgres" | "postgresql" => Ok(Self::new(PostgresDriver::open_with_pool(
                url,
                pool.unwrap_or(postgres::DEFAULT_POOL),
            )?)),

            // Драйвер відомий, але не увімкнений при збірці — кажемо, який
            // feature додати, а не «невідомий драйвер».
            #[cfg(not(feature = "sqlite"))]
            "sqlite" => Err(Self::disabled("sqlite")),
            #[cfg(not(feature = "postgres"))]
            "postgres" | "postgresql" => Err(Self::disabled("postgres")),

            // Mongo й Surreal — далі; трейт до них уже готовий.
            other => Err(DbError::Config(format!(
                "невідомий драйвер `{other}`; підтримуються `sqlite` і `postgres`"
            ))),
        }
    }

    /// Драйвер відомий rhaix, але цей бінарник зібрано без нього.
    #[allow(dead_code)]
    fn disabled(name: &str) -> DbError {
        DbError::Config(format!(
            "драйвер `{name}` не увімкнено в цій збірці; додайте feature `{name}` для rhaix-db (у проді це робить `rhaix build` за секцією [db])"
        ))
    }

    pub fn driver(&self) -> &Arc<dyn DbDriver> {
        &self.0
    }

    pub fn kind(&self) -> &'static str {
        self.0.kind()
    }

    pub fn transaction(
        &self,
        body: &mut dyn FnMut(Arc<dyn DbDriver>) -> Result<Dynamic, DbError>,
    ) -> Result<Dynamic, DbError> {
        self.0.transaction(body)
    }

    /// Застосувати вже прочитані міграції (ім'я, SQL).
    ///
    /// Саме цим користується сервер: у розробці файли читаються з диска, у
    /// зібраному бінарнику — з вшитої таблиці, а база про різницю не знає.
    pub fn migrate(&self, migrations: &[(String, String)]) -> Result<Vec<String>, DbError> {
        let mut sorted = migrations.to_vec();
        // Порядок — за іменем файлу, тому нумерація `001_`, `002_` обов'язкова.
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        self.0.migrate(&sorted)
    }

    /// Прочитати `migrations/*.sql` і застосувати їх по порядку імен.
    pub fn migrate_from(&self, dir: &Path) -> Result<Vec<String>, DbError> {
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut files: Vec<(String, String)> = Vec::new();
        let entries = std::fs::read_dir(dir)
            .map_err(|err| DbError::Config(format!("не вдалося прочитати міграції: {err}")))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("sql") {
                continue;
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let body = std::fs::read_to_string(&path)
                .map_err(|err| DbError::Config(format!("міграція `{name}`: {err}")))?;
            files.push((name, body));
        }
        self.migrate(&files)
    }
}

impl fmt::Debug for Database {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Database({})", self.0.kind())
    }
}

#[cfg(test)]
mod feature_tests {
    use super::*;

    #[test]
    #[cfg(not(feature = "postgres"))]
    fn a_disabled_driver_names_the_feature_to_enable() {
        // Збірка без postgres, але конфіг просить його: помилка має підказати,
        // який feature додати, а не «невідомий драйвер».
        let err = Database::open("postgres", "postgres://x").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("postgres"), "{text}");
        assert!(text.contains("feature"), "{text}");
    }

    #[test]
    fn an_unknown_driver_is_still_unknown() {
        let err = Database::open("oracle", "x").unwrap_err();
        assert!(err.to_string().contains("невідомий драйвер"), "{err}");
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Зручний конструктор мапи для тестів драйвера.
    pub fn map(pairs: &[(&str, Dynamic)]) -> Map {
        let mut out = Map::new();
        for (key, value) in pairs {
            out.insert((*key).into(), value.clone());
        }
        out
    }

    fn database() -> Database {
        let db = Database::open("sqlite", ":memory:").expect("база в пам'яті");
        db.driver()
            .raw_exec(
                "create table orders (id integer primary key, title text, qty integer, done integer)",
                &[],
            )
            .expect("таблиця");
        db
    }

    #[test]
    fn unconfigured_database_explains_itself() {
        let db = Database::unconfigured();
        let err = db
            .driver()
            .find("todos", &Map::new(), &Map::new())
            .unwrap_err();
        assert!(err.to_string().contains("rhaix.toml"), "{err}");
        assert!(err.to_string().contains("driver"), "{err}");
    }

    #[test]
    fn unknown_driver_says_what_is_supported() {
        let err = Database::open("oracle", "").unwrap_err();
        assert!(err.to_string().contains("sqlite"), "{err}");
    }

    #[test]
    fn portable_crud_round_trip() {
        let db = database();
        let driver = db.driver();

        let created = driver
            .insert(
                "orders",
                &map(&[
                    ("title", Dynamic::from("перше")),
                    ("qty", Dynamic::from(3_i64)),
                    ("done", Dynamic::from(false)),
                ]),
            )
            .unwrap();
        driver
            .insert(
                "orders",
                &map(&[
                    ("title", Dynamic::from("друге")),
                    ("qty", Dynamic::from(30_i64)),
                    ("done", Dynamic::from(true)),
                ]),
            )
            .unwrap();

        assert_eq!(driver.count("orders", &Map::new()).unwrap(), 2);
        assert_eq!(
            driver
                .count("orders", &map(&[("done", Dynamic::from(true))]))
                .unwrap(),
            1
        );

        let found = driver
            .find(
                "orders",
                &map(&[(
                    "qty",
                    Dynamic::from_map(map(&[("gte", Dynamic::from(10_i64))])),
                )]),
                &Map::new(),
            )
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["title"].to_string(), "друге");

        let one = driver
            .get("orders", Dynamic::from(created.last_id))
            .unwrap()
            .expect("запис на місці");
        assert_eq!(one["title"].to_string(), "перше");

        driver
            .update(
                "orders",
                Dynamic::from(created.last_id),
                &map(&[("done", Dynamic::from(true))]),
            )
            .unwrap();
        assert_eq!(
            driver
                .count("orders", &map(&[("done", Dynamic::from(true))]))
                .unwrap(),
            2
        );

        driver
            .delete("orders", Dynamic::from(created.last_id))
            .unwrap();
        assert_eq!(driver.count("orders", &Map::new()).unwrap(), 1);
    }

    #[test]
    fn sorting_and_paging_work_end_to_end() {
        let db = database();
        let driver = db.driver();
        for index in 1..=5 {
            driver
                .insert(
                    "orders",
                    &map(&[
                        ("title", Dynamic::from(format!("№{index}"))),
                        ("qty", Dynamic::from(index as i64)),
                    ]),
                )
                .unwrap();
        }

        let page = driver
            .find(
                "orders",
                &Map::new(),
                &map(&[
                    ("sort", Dynamic::from("qty desc")),
                    ("limit", Dynamic::from(2_i64)),
                    ("skip", Dynamic::from(1_i64)),
                ]),
            )
            .unwrap();
        let titles: Vec<String> = page.iter().map(|row| row["title"].to_string()).collect();
        assert_eq!(titles, vec!["№4".to_owned(), "№3".to_owned()]);
    }

    #[test]
    fn injection_attempt_in_a_table_name_is_refused() {
        let db = database();
        let err = db
            .driver()
            .find("orders; drop table orders", &Map::new(), &Map::new())
            .unwrap_err();
        assert!(err.to_string().contains("недопустиме ім'я"), "{err}");
        // таблиця на місці
        assert_eq!(db.driver().count("orders", &Map::new()).unwrap(), 0);
    }
}
