//! Драйвер PostgreSQL.
//!
//! Переносимий шар (`db.find/insert/...`) сюди приходить готовим SQL із
//! плейсхолдерами `?` — так само, як до SQLite. Postgres чекає `$1, $2, …`,
//! тому драйвер переписує плейсхолдери перед виконанням. Це єдина відмінність,
//! яку помітно ззовні: усе решта — той самий трейт [`DbDriver`].
//!
//! З'єднання беруться з невеликого пулу: рендер іде в `spawn_blocking`, тож
//! паралельних запитів рівно стільки, скільки потоків.

use std::sync::{Arc, Mutex};

use postgres::types::{FromSql, ToSql, Type};
use postgres::{Client, NoTls, Row};
use rhai::{Dynamic, Map};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use crate::{Affected, DbDriver, DbError};

/// Скільки з'єднань тримати відкритими.
const POOL_SIZE: usize = 8;

pub struct PostgresDriver {
    url: String,
    pool: Mutex<Vec<Client>>,
}

impl PostgresDriver {
    /// `url` — рядок під'єднання: `postgres://user:pass@host:port/db`.
    pub fn open(url: &str) -> Result<Arc<Self>, DbError> {
        let driver = Arc::new(Self {
            url: url.to_owned(),
            pool: Mutex::new(Vec::new()),
        });
        // Перевіряємо одразу: краще впасти на старті, ніж на першому запиті.
        let client = driver.connect()?;
        driver.checkin(client);
        Ok(driver)
    }

    fn connect(&self) -> Result<Client, DbError> {
        Client::connect(&self.url, NoTls)
            .map_err(|err| DbError::Config(format!("не вдалося під'єднатися до postgres: {err}")))
    }

    fn checkout(&self) -> Result<Client, DbError> {
        if let Some(client) = self.pool.lock().expect("пул не отруєний").pop() {
            return Ok(client);
        }
        self.connect()
    }

    fn checkin(&self, client: Client) {
        let mut pool = self.pool.lock().expect("пул не отруєний");
        if pool.len() < POOL_SIZE {
            pool.push(client);
        }
    }

    /// Взяти з'єднання, зробити з ним щось і повернути в пул.
    ///
    /// Якщо з'єднання зламалось (мережа впала), у пул воно не повертається —
    /// наступний виклик відкриє свіже.
    fn with<T>(&self, f: impl FnOnce(&mut Client) -> Result<T, DbError>) -> Result<T, DbError> {
        let mut client = self.checkout()?;
        let broken = client.is_closed();
        let result = f(&mut client);
        if !broken && !client.is_closed() {
            self.checkin(client);
        }
        result
    }
}

impl DbDriver for PostgresDriver {
    fn kind(&self) -> &'static str {
        "postgres"
    }

    fn raw_query(&self, sql: &str, params: &[Dynamic]) -> Result<Vec<Map>, DbError> {
        let sql = to_dollar_placeholders(sql);
        self.with(|client| run_query(client, &sql, params))
    }

    fn raw_exec(&self, sql: &str, params: &[Dynamic]) -> Result<Affected, DbError> {
        let sql = to_dollar_placeholders(sql);
        self.with(|client| run_exec(client, &sql, params))
    }

    /// `insert` у Postgres не віддає новий id через `execute` — потрібен
    /// `RETURNING`. Тому переносимий вставку перевизначаємо: додаємо
    /// `returning "id"` і читаємо його як результат запиту.
    fn insert(&self, table: &str, values: &Map) -> Result<Affected, DbError> {
        let built = crate::query::insert(table, values)?;
        let sql = format!(
            "{} returning \"{}\"",
            to_dollar_placeholders(&built.text),
            crate::query::PRIMARY_KEY
        );
        self.with(|client| {
            let rows = run_query(client, &sql, &built.params)?;
            let last_id = rows
                .first()
                .and_then(|row| row.get(crate::query::PRIMARY_KEY))
                .and_then(|value| value.as_int().ok())
                .unwrap_or(0);
            Ok(Affected { rows: 1, last_id })
        })
    }

    fn transaction(
        &self,
        body: &mut dyn FnMut(Arc<dyn DbDriver>) -> Result<Dynamic, DbError>,
    ) -> Result<Dynamic, DbError> {
        let mut client = self.checkout()?;
        client
            .batch_execute("begin")
            .map_err(|err| DbError::Query(format!("не вдалося почати транзакцію: {err}")))?;

        let pinned = Arc::new(PinnedPostgres {
            client: Mutex::new(Some(client)),
        });
        let result = body(pinned.clone());

        let Some(mut client) = pinned.take() else {
            return Err(DbError::Query(
                "з'єднання транзакції лишилось зайнятим: не зберігайте `t` поза межами db.tx"
                    .into(),
            ));
        };
        match &result {
            Ok(_) => client
                .batch_execute("commit")
                .map_err(|err| DbError::Query(format!("не вдалося завершити транзакцію: {err}")))?,
            Err(_) => {
                let _ = client.batch_execute("rollback");
            }
        }
        if !client.is_closed() {
            self.checkin(client);
        }
        result
    }

    fn migrate(&self, migrations: &[(String, String)]) -> Result<Vec<String>, DbError> {
        self.with(|client| {
            client
                .batch_execute(
                    "create table if not exists _rhaix_migrations (
                         name text primary key,
                         applied_at timestamptz not null default now()
                     )",
                )
                .map_err(|err| {
                    DbError::Query(format!("не вдалося створити таблицю міграцій: {err}"))
                })?;

            let mut applied = Vec::new();
            for (name, sql) in migrations {
                let already = client
                    .query_one(
                        "select count(*) as n from _rhaix_migrations where name = $1",
                        &[name],
                    )
                    .map(|row| row.get::<_, i64>("n"))
                    .map_err(|err| DbError::Query(err.to_string()))?;
                if already > 0 {
                    continue;
                }
                // Кожна міграція — одна транзакція: або вся, або жодної.
                let mut tx = client
                    .transaction()
                    .map_err(|err| DbError::Query(format!("міграція `{name}`: {err}")))?;
                tx.batch_execute(sql)
                    .map_err(|err| DbError::Query(format!("міграція `{name}`: {err}")))?;
                tx.execute("insert into _rhaix_migrations (name) values ($1)", &[name])
                    .map_err(|err| DbError::Query(format!("міграція `{name}`: {err}")))?;
                tx.commit()
                    .map_err(|err| DbError::Query(format!("міграція `{name}`: {err}")))?;
                applied.push(name.clone());
            }
            Ok(applied)
        })
    }
}

/// Драйвер, прив'язаний до одного з'єднання — усередині `db.tx`.
struct PinnedPostgres {
    client: Mutex<Option<Client>>,
}

impl PinnedPostgres {
    fn take(&self) -> Option<Client> {
        self.client.lock().expect("з'єднання не отруєне").take()
    }

    fn with<T>(&self, f: impl FnOnce(&mut Client) -> Result<T, DbError>) -> Result<T, DbError> {
        let mut guard = self.client.lock().expect("з'єднання не отруєне");
        match guard.as_mut() {
            Some(client) => f(client),
            None => Err(DbError::Query("транзакцію вже завершено".into())),
        }
    }
}

impl DbDriver for PinnedPostgres {
    fn kind(&self) -> &'static str {
        "postgres"
    }

    fn raw_query(&self, sql: &str, params: &[Dynamic]) -> Result<Vec<Map>, DbError> {
        let sql = to_dollar_placeholders(sql);
        self.with(|client| run_query(client, &sql, params))
    }

    fn raw_exec(&self, sql: &str, params: &[Dynamic]) -> Result<Affected, DbError> {
        let sql = to_dollar_placeholders(sql);
        self.with(|client| run_exec(client, &sql, params))
    }

    fn insert(&self, table: &str, values: &Map) -> Result<Affected, DbError> {
        let built = crate::query::insert(table, values)?;
        let sql = format!(
            "{} returning \"{}\"",
            to_dollar_placeholders(&built.text),
            crate::query::PRIMARY_KEY
        );
        self.with(|client| {
            let rows = run_query(client, &sql, &built.params)?;
            let last_id = rows
                .first()
                .and_then(|row| row.get(crate::query::PRIMARY_KEY))
                .and_then(|value| value.as_int().ok())
                .unwrap_or(0);
            Ok(Affected { rows: 1, last_id })
        })
    }
}

// ------------------------------------------------------------- виконання

fn run_query(client: &mut Client, sql: &str, params: &[Dynamic]) -> Result<Vec<Map>, DbError> {
    let bound: Vec<Bind> = params.iter().map(bind).collect();
    let refs: Vec<&(dyn postgres::types::ToSql + Sync)> = bound
        .iter()
        .map(|b| b as &(dyn postgres::types::ToSql + Sync))
        .collect();
    let rows = client
        .query(sql, &refs)
        .map_err(|err| query_error(sql, err))?;
    Ok(rows.iter().map(row_to_map).collect())
}

fn run_exec(client: &mut Client, sql: &str, params: &[Dynamic]) -> Result<Affected, DbError> {
    let bound: Vec<Bind> = params.iter().map(bind).collect();
    let refs: Vec<&(dyn postgres::types::ToSql + Sync)> = bound
        .iter()
        .map(|b| b as &(dyn postgres::types::ToSql + Sync))
        .collect();
    let rows = client
        .execute(sql, &refs)
        .map_err(|err| query_error(sql, err))?;
    // У Postgres немає «останнього rowid»: id повертає лише `insert` через
    // `RETURNING` (див. вище). Для `update`/`delete` це поле й не потрібне.
    Ok(Affected {
        rows: rows as i64,
        last_id: 0,
    })
}

fn row_to_map(row: &Row) -> Map {
    let mut map = Map::new();
    for (index, column) in row.columns().iter().enumerate() {
        map.insert(column.name().into(), value_of(row, index, column.type_()));
    }
    map
}

/// Прочитати одну комірку в `Dynamic` за типом колонки.
///
/// Дата/час і `numeric` приходять як рядок і число відповідно — так само, як
/// їх бачить `date()` і `money()` на боці SQLite, тому решта rhaix різниці не
/// помічає.
fn value_of(row: &Row, index: usize, ty: &Type) -> Dynamic {
    use postgres::types::Type as T;

    match *ty {
        T::BOOL => opt::<bool>(row, index)
            .map(Dynamic::from)
            .unwrap_or(Dynamic::UNIT),
        T::INT2 => opt::<i16>(row, index)
            .map(|n| Dynamic::from(n as i64))
            .unwrap_or(Dynamic::UNIT),
        T::INT4 => opt::<i32>(row, index)
            .map(|n| Dynamic::from(n as i64))
            .unwrap_or(Dynamic::UNIT),
        T::INT8 => opt::<i64>(row, index)
            .map(Dynamic::from)
            .unwrap_or(Dynamic::UNIT),
        T::OID => opt::<u32>(row, index)
            .map(|n| Dynamic::from(n as i64))
            .unwrap_or(Dynamic::UNIT),
        T::FLOAT4 => opt::<f32>(row, index)
            .map(|n| Dynamic::from(n as f64))
            .unwrap_or(Dynamic::UNIT),
        T::FLOAT8 => opt::<f64>(row, index)
            .map(Dynamic::from)
            .unwrap_or(Dynamic::UNIT),
        T::NUMERIC => opt::<Decimal>(row, index)
            // rhaix не має десяткового типу — приводимо до f64, як і SQLite REAL.
            .and_then(|d| d.to_f64())
            .map(Dynamic::from)
            .unwrap_or(Dynamic::UNIT),
        T::TEXT | T::VARCHAR | T::BPCHAR | T::NAME | T::UNKNOWN => opt::<String>(row, index)
            .map(Dynamic::from)
            .unwrap_or(Dynamic::UNIT),
        T::UUID => opt::<uuid::Uuid>(row, index)
            .map(|u| Dynamic::from(u.to_string()))
            .unwrap_or(Dynamic::UNIT),
        T::TIMESTAMP => opt::<chrono::NaiveDateTime>(row, index)
            .map(|t| Dynamic::from(t.format("%Y-%m-%d %H:%M:%S").to_string()))
            .unwrap_or(Dynamic::UNIT),
        T::TIMESTAMPTZ => opt::<chrono::DateTime<chrono::Utc>>(row, index)
            .map(|t| Dynamic::from(t.format("%Y-%m-%dT%H:%M:%SZ").to_string()))
            .unwrap_or(Dynamic::UNIT),
        T::DATE => opt::<chrono::NaiveDate>(row, index)
            .map(|d| Dynamic::from(d.format("%Y-%m-%d").to_string()))
            .unwrap_or(Dynamic::UNIT),
        T::TIME => opt::<chrono::NaiveTime>(row, index)
            .map(|t| Dynamic::from(t.format("%H:%M:%S").to_string()))
            .unwrap_or(Dynamic::UNIT),
        T::JSON | T::JSONB => opt::<serde_json::Value>(row, index)
            .map(json_to_dynamic)
            .unwrap_or(Dynamic::UNIT),
        T::BYTEA => opt::<Vec<u8>>(row, index)
            .map(Dynamic::from_blob)
            .unwrap_or(Dynamic::UNIT),
        // Невідомий тип: пробуємо як рядок, а не мовчазний NULL — так людина
        // побачить значення й зрозуміє, що бракує підтримки типу.
        _ => opt::<String>(row, index)
            .map(Dynamic::from)
            .unwrap_or(Dynamic::UNIT),
    }
}

/// `row.get`, який не панікує на NULL і на несподіваному типі.
fn opt<'a, T: FromSql<'a>>(row: &'a Row, index: usize) -> Option<T> {
    row.try_get::<usize, Option<T>>(index).ok().flatten()
}

/// JSON → значення rhai. Дублює логіку `rhaix-script::json`, але цей крейт від
/// нього не залежить, тому конвертер тут свій, невеликий.
fn json_to_dynamic(value: serde_json::Value) -> Dynamic {
    use serde_json::Value;
    match value {
        Value::Null => Dynamic::UNIT,
        Value::Bool(b) => Dynamic::from(b),
        Value::Number(n) => n
            .as_i64()
            .map(Dynamic::from)
            .or_else(|| n.as_f64().map(Dynamic::from))
            .unwrap_or(Dynamic::UNIT),
        Value::String(s) => Dynamic::from(s),
        Value::Array(items) => {
            Dynamic::from_array(items.into_iter().map(json_to_dynamic).collect())
        }
        Value::Object(fields) => {
            let mut map = Map::new();
            for (key, item) in fields {
                map.insert(key.into(), json_to_dynamic(item));
            }
            Dynamic::from_map(map)
        }
    }
}

// ------------------------------------------------------------- параметри

/// Значення rhai, готове піти в Postgres як параметр.
///
/// Типи звужені до того, що вміє переносимий шар: `()`, bool, ціле, дійсне,
/// рядок. Мапи й масиви їдуть текстом — так само, як у SQLite, щоб людина
/// побачила, що саме поїхало, а не мовчазний NULL.
#[derive(Debug)]
enum Bind {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
}

fn bind(value: &Dynamic) -> Bind {
    if value.is_unit() {
        return Bind::Null;
    }
    if let Some(flag) = value.clone().try_cast::<bool>() {
        return Bind::Bool(flag);
    }
    if let Ok(number) = value.as_int() {
        return Bind::Int(number);
    }
    if let Ok(number) = value.as_float() {
        return Bind::Float(number);
    }
    if let Some(blob) = value.read_lock::<rhai::Blob>() {
        return Bind::Bytes(blob.clone());
    }
    if let Some(text) = value.read_lock::<rhai::ImmutableString>() {
        return Bind::Text(text.to_string());
    }
    Bind::Text(value.to_string())
}

impl postgres::types::ToSql for Bind {
    fn to_sql(
        &self,
        ty: &Type,
        out: &mut postgres::types::private::BytesMut,
    ) -> Result<postgres::types::IsNull, Box<dyn std::error::Error + Sync + Send>> {
        match self {
            Bind::Null => Ok(postgres::types::IsNull::Yes),
            Bind::Bool(v) => v.to_sql(ty, out),
            Bind::Int(v) => int_to_sql(*v, ty, out),
            Bind::Float(v) => float_to_sql(*v, ty, out),
            Bind::Text(v) => text_to_sql(v, ty, out),
            Bind::Bytes(v) => v.to_sql(ty, out),
        }
    }

    fn accepts(_ty: &Type) -> bool {
        // Приймаємо будь-який тип: справжню перевірку робить `to_sql` за
        // фактичним типом колонки.
        true
    }

    postgres::types::to_sql_checked!();
}

/// Рядок rhai у колонку — з приведенням до її типу.
///
/// Найважливіша коерція переносимого шару: `req.param("id")` і `req.form(...)`
/// завжди повертають рядок, а колонка `id` — `integer`. SQLite приймає `"4"`
/// у числову колонку сам; Postgres суворий, тому рядок, що виглядає числом,
/// перетворюємо на число, перш ніж він поїде в порівняння чи `insert`.
fn text_to_sql(
    value: &str,
    ty: &Type,
    out: &mut postgres::types::private::BytesMut,
) -> Result<postgres::types::IsNull, Box<dyn std::error::Error + Sync + Send>> {
    use rust_decimal::prelude::FromPrimitive;
    match *ty {
        Type::INT2 => value.trim().parse::<i16>()?.to_sql(ty, out),
        Type::INT4 => value.trim().parse::<i32>()?.to_sql(ty, out),
        Type::INT8 => value.trim().parse::<i64>()?.to_sql(ty, out),
        Type::FLOAT4 => value.trim().parse::<f32>()?.to_sql(ty, out),
        Type::FLOAT8 => value.trim().parse::<f64>()?.to_sql(ty, out),
        Type::NUMERIC => {
            let number = value.trim().parse::<f64>()?;
            Decimal::from_f64(number)
                .unwrap_or_default()
                .to_sql(ty, out)
        }
        Type::BOOL => matches!(value.trim(), "true" | "t" | "1" | "yes" | "on").to_sql(ty, out),
        // text/varchar та решта — як є.
        _ => value.to_sql(ty, out),
    }
}

/// Дійсне rhai у колонку: `numeric` чекає `Decimal`, цілі колонки — округлення.
fn float_to_sql(
    value: f64,
    ty: &Type,
    out: &mut postgres::types::private::BytesMut,
) -> Result<postgres::types::IsNull, Box<dyn std::error::Error + Sync + Send>> {
    use rust_decimal::prelude::FromPrimitive;
    match *ty {
        Type::NUMERIC => Decimal::from_f64(value).unwrap_or_default().to_sql(ty, out),
        Type::FLOAT4 => (value as f32).to_sql(ty, out),
        Type::INT2 => (value.round() as i16).to_sql(ty, out),
        Type::INT4 => (value.round() as i32).to_sql(ty, out),
        Type::INT8 => (value.round() as i64).to_sql(ty, out),
        _ => value.to_sql(ty, out),
    }
}

/// Ціле rhai (`i64`) підганяємо під фактичний тип колонки: `int4` чекає `i32`,
/// `bool` — булеве. Інакше `insert` у колонку `integer` падав би з «i64 vs int4».
fn int_to_sql(
    value: i64,
    ty: &Type,
    out: &mut postgres::types::private::BytesMut,
) -> Result<postgres::types::IsNull, Box<dyn std::error::Error + Sync + Send>> {
    match *ty {
        Type::INT2 => (value as i16).to_sql(ty, out),
        Type::INT4 => (value as i32).to_sql(ty, out),
        Type::FLOAT4 => (value as f32).to_sql(ty, out),
        Type::FLOAT8 => (value as f64).to_sql(ty, out),
        Type::BOOL => (value != 0).to_sql(ty, out),
        Type::NUMERIC => Decimal::from(value).to_sql(ty, out),
        // int8 та решта — як є.
        _ => value.to_sql(ty, out),
    }
}

/// Переписати `?`-плейсхолдери на `$1, $2, …`, не чіпаючи `?` усередині
/// рядкових літералів (`'...'`, з екрануванням `''`) та ідентифікаторів
/// (`"..."`).
///
/// Переносимий шар генерує тільки прості `?`, але `db.query(...)` користувач
/// пише рукою, і `where note = 'a?b'` не має перетворитись на параметр.
fn to_dollar_placeholders(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len() + 8);
    let mut n = 0;
    let bytes = sql.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;

    while i < bytes.len() {
        let ch = bytes[i];
        match quote {
            Some(q) => {
                out.push(ch as char);
                if ch == q {
                    // Подвоєна лапка всередині — це екранування, не кінець.
                    if i + 1 < bytes.len() && bytes[i + 1] == q {
                        out.push(q as char);
                        i += 2;
                        continue;
                    }
                    quote = None;
                }
                i += 1;
            }
            None => match ch {
                b'\'' | b'"' => {
                    quote = Some(ch);
                    out.push(ch as char);
                    i += 1;
                }
                b'?' => {
                    n += 1;
                    out.push('$');
                    out.push_str(&n.to_string());
                    i += 1;
                }
                _ => {
                    out.push(ch as char);
                    i += 1;
                }
            },
        }
    }
    out
}

fn query_error(sql: &str, err: postgres::Error) -> DbError {
    // `postgres::Error` у Display ховає суть за «db error»; сам текст помилки
    // лежить у as_db_error() — саме він потрібен людині.
    let detail = err
        .as_db_error()
        .map(|db| db.message().to_owned())
        .unwrap_or_else(|| err.to_string());
    DbError::Query(format!("{detail}\n  запит: {}", sql.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_become_dollars_outside_strings() {
        assert_eq!(
            to_dollar_placeholders("select * from t where a = ? and b = ?"),
            "select * from t where a = $1 and b = $2"
        );
        // `?` усередині рядка лишається собою.
        assert_eq!(
            to_dollar_placeholders("select * from t where note = 'a?b' and x = ?"),
            "select * from t where note = 'a?b' and x = $1"
        );
        // Подвоєна лапка — екранування всередині рядка.
        assert_eq!(
            to_dollar_placeholders("select 'it''s ?' , ?"),
            "select 'it''s ?' , $1"
        );
    }

    #[test]
    fn json_nesting_survives() {
        let value = json_to_dynamic(serde_json::json!({"a": [1, 2], "b": {"c": "д"}}));
        let map = value.cast::<Map>();
        assert_eq!(map["a"].clone().cast::<rhai::Array>().len(), 2);
        let inner = map["b"].clone().cast::<Map>();
        assert_eq!(inner["c"].clone().cast::<String>(), "д");
    }
}

// Наскрізні тести драйвера. Вони йдуть лише коли задано RHAIX_PG_TEST_URL —
// інакше на машині без Postgres `cargo test` просто їх пропускає.
#[cfg(test)]
mod live_tests {
    use super::*;
    use rhai::Array;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// Усі live-тести ділять одну базу й ті самі таблиці, тож ідуть по черзі:
    /// інакше `drop`/`create` одного топче схему іншого.
    fn serial() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn driver() -> Option<(Arc<PostgresDriver>, MutexGuard<'static, ()>)> {
        let guard = serial();
        let url = std::env::var("RHAIX_PG_TEST_URL").ok()?;
        let db = PostgresDriver::open(&url).expect("тестова база має бути доступна");
        // Чиста схема на кожен запуск.
        db.raw_exec("drop table if exists orders", &[]).unwrap();
        db.raw_exec("drop table if exists _rhaix_migrations", &[])
            .unwrap();
        Some((db, guard))
    }

    fn seed(db: &PostgresDriver) {
        db.migrate(&[(
            "001".to_owned(),
            "create table orders (
                 id serial primary key,
                 customer text not null,
                 amount numeric(12,2) not null default 0,
                 done boolean not null default false,
                 created timestamptz not null default now()
             )"
            .to_owned(),
        )])
        .expect("міграція");
    }

    #[test]
    fn portable_crud_round_trips() {
        let Some((db, _guard)) = driver() else { return };
        seed(&db);

        // insert повертає новий id через RETURNING.
        let id = db
            .insert(
                "orders",
                &map(&[("customer", "Оля".into()), ("amount", 1234.5.into())]),
            )
            .expect("insert")
            .last_id;
        assert!(id > 0, "insert має повернути id");

        db.insert(
            "orders",
            &map(&[("customer", "Петро".into()), ("done", true.into())]),
        )
        .expect("insert 2");

        // get за первинним ключем.
        let one = db
            .get("orders", Dynamic::from(id))
            .expect("get")
            .expect("є запис");
        assert_eq!(one["customer"].clone().cast::<String>(), "Оля");
        // numeric приходить як f64, як і REAL у SQLite.
        assert_eq!(one["amount"].clone().cast::<f64>(), 1234.5);
        // timestamptz — рядок ISO, який розуміє date().
        assert!(
            one["created"].clone().cast::<String>().contains('T'),
            "{one:?}"
        );

        // find зі словником операторів + сортування.
        let rows = db
            .find(
                "orders",
                &map(&[("amount", map(&[("gte", 1000.0.into())]).into())]),
                &map(&[("sort", "id desc".into())]),
            )
            .expect("find");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["customer"].clone().cast::<String>(), "Оля");

        // count.
        assert_eq!(db.count("orders", &Map::new()).expect("count"), 2);

        // update + delete.
        db.update("orders", Dynamic::from(id), &map(&[("done", true.into())]))
            .expect("update");
        let done = db
            .count("orders", &map(&[("done", true.into())]))
            .expect("count done");
        assert_eq!(done, 2);

        db.delete("orders", Dynamic::from(id)).expect("delete");
        assert_eq!(
            db.count("orders", &Map::new()).expect("count after delete"),
            1
        );
    }

    #[test]
    fn a_transaction_rolls_back_on_error() {
        let Some((db, _guard)) = driver() else { return };
        seed(&db);

        let result = db.transaction(&mut |tx| {
            tx.insert("orders", &map(&[("customer", "перший".into())]))?;
            // Друга вставка падає (колонки nosuch немає) — перша має відкотитись.
            tx.raw_exec("insert into orders (nosuch) values (1)", &[])?;
            Ok(Dynamic::UNIT)
        });
        assert!(result.is_err());
        assert_eq!(db.count("orders", &Map::new()).expect("count"), 0);

        // А успішна транзакція фіксується.
        let _ = db
            .transaction(&mut |tx| {
                tx.insert("orders", &map(&[("customer", "другий".into())]))?;
                Ok(Dynamic::UNIT)
            })
            .expect("транзакція");
        assert_eq!(db.count("orders", &Map::new()).expect("count"), 1);
    }

    #[test]
    fn migrations_apply_once() {
        let Some((db, _guard)) = driver() else { return };
        let m = [(
            "001".to_owned(),
            "create table orders (id serial primary key)".to_owned(),
        )];
        assert_eq!(db.migrate(&m).expect("перший раз").len(), 1);
        // Другий прогін нічого не застосовує.
        assert_eq!(db.migrate(&m).expect("другий раз").len(), 0);
    }

    #[test]
    fn a_string_id_is_coerced_to_the_column_type() {
        // req.param("id") завжди рядок; колонка id — integer. У SQLite це
        // працює саме собою, у Postgres рядок треба привести — інакше запит
        // падав із «insufficient data left in message».
        let Some((db, _guard)) = driver() else { return };
        seed(&db);
        let id = db
            .insert("orders", &map(&[("customer", "Ключ".into())]))
            .expect("insert")
            .last_id;

        // get із рядковим id, як його віддає маршрут.
        let found = db
            .get("orders", Dynamic::from(id.to_string()))
            .expect("get")
            .expect("є запис");
        assert_eq!(found["customer"].clone().cast::<String>(), "Ключ");

        // update і delete теж приймають рядковий id.
        db.update(
            "orders",
            Dynamic::from(id.to_string()),
            &map(&[("customer", "Змінено".into())]),
        )
        .expect("update");
        db.delete("orders", Dynamic::from(id.to_string()))
            .expect("delete");
        assert_eq!(db.count("orders", &Map::new()).expect("count"), 0);
    }

    #[test]
    fn native_query_uses_question_placeholders() {
        let Some((db, _guard)) = driver() else { return };
        seed(&db);
        db.insert("orders", &map(&[("customer", "Ігор".into())]))
            .unwrap();

        // Той самий `?`, що й для SQLite — драйвер перекладає на $1.
        let rows = db
            .raw_query(
                "select customer from orders where customer = ?",
                &[Dynamic::from("Ігор".to_owned())],
            )
            .expect("query");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["customer"].clone().cast::<String>(), "Ігор");
    }

    fn map(pairs: &[(&str, Dynamic)]) -> Map {
        pairs
            .iter()
            .map(|(k, v)| ((*k).into(), v.clone()))
            .collect()
    }

    // Тримаємо Array у використанні, щоб імпорт не був "невикористаним" у
    // збірках, де цей модуль компілюється без запуску.
    #[allow(dead_code)]
    fn _touch(a: Array) -> usize {
        a.len()
    }
}
