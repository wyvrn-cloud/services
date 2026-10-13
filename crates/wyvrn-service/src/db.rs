//! Where a service keeps what it keeps: a SQLite file, or a MySQL (or MariaDB)
//! database it is given the address of.
//!
//! A service writes its statements once, in the SQL the two have in common, with
//! numbered parameters (`?1`), and this module says them to whichever it has. What
//! differs is put right here (see [`for_mysql`]); what can't be is kept out of the
//! statements: no `ON CONFLICT`, no two-argument `MIN`/`MAX`, no reading a table in
//! the `VALUES` of an insert into it. The tables themselves are written out for
//! each, since their column types are not alike ([`Schema`]).
//!
//! One connection, behind a lock, for either: a service's store has always been
//! one thing at a time, and several of its methods are several statements that
//! count on nothing coming between them.
//!
//! A deployment says which with `<SERVICE>_DATABASE_URL` or `DATABASE_URL`
//! ([`Location::from_env`]): `mysql://user:password@host:3306/database`. Each
//! service needs a database of its own: their tables share names.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use mysql::prelude::Queryable;

/// Where a store is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Sqlite(PathBuf),
    /// `mysql://user:password@host:port/database`.
    MySql(String),
}

impl Location {
    /// What the environment says, for the service whose settings start with
    /// `prefix` (`COMMUNITY`): `<prefix>_DATABASE_URL`, else `DATABASE_URL`, else
    /// the SQLite file at `path`. A URL is `mysql://…` (or `mariadb://…`), or
    /// `sqlite://<path>` to say the file another way.
    pub fn from_env(prefix: &str, path: &Path) -> Result<Self> {
        let named = |name: &str| std::env::var(name).ok().map(|url| url.trim().to_string()).filter(|url| !url.is_empty());
        match named(&format!("{prefix}_DATABASE_URL")).or_else(|| named("DATABASE_URL")) {
            Some(url) => Self::parse(&url),
            None => Ok(Self::Sqlite(path.to_path_buf())),
        }
    }

    pub fn parse(url: &str) -> Result<Self> {
        if let Some(rest) = url.strip_prefix("mariadb://") {
            return Ok(Self::MySql(format!("mysql://{rest}")));
        }
        if url.starts_with("mysql://") {
            return Ok(Self::MySql(url.to_string()));
        }
        if let Some(path) = url.strip_prefix("sqlite://").or_else(|| url.strip_prefix("sqlite:")) {
            return Ok(Self::Sqlite(PathBuf::from(path)));
        }
        Err(Error::Other(format!("a database URL starts with mysql://, mariadb:// or sqlite://, and this one doesn't ({})", Self::without_password(url))))
    }

    fn without_password(url: &str) -> String {
        match (url.find("://"), url.rfind('@')) {
            (Some(scheme), Some(at)) if at > scheme => format!("{}…{}", &url[..scheme + 3], &url[at..]),
            _ => url.to_string(),
        }
    }
}

/// As it is safe to print: without the password.
impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(path) => write!(f, "{}", path.display()),
            Self::MySql(url) => write!(f, "{}", Self::without_password(url)),
        }
    }
}

/// A service's tables, written for each kind of database. Every statement is
/// `CREATE … IF NOT EXISTS`: they are run each time the store is opened.
///
/// For MySQL: a column a row is found by is a `VARCHAR` (a `TEXT` can't be a
/// key), and one that holds a DID is [`MYSQL_DID`]. The tables are made
/// case-sensitive, as SQLite's are.
pub struct Schema {
    pub sqlite: &'static str,
    pub mysql: &'static str,
}

/// A MySQL column that holds a DID. DIDs are long (a `did:peer:4` in full runs
/// past a thousand characters) and always ASCII, and an index entry is at most
/// 3072 bytes: so one byte a character, and room for most of that.
pub const MYSQL_DID: &str = "VARCHAR(3000) CHARACTER SET ascii COLLATE ascii_bin";

/// What follows a MySQL table's columns.
pub const MYSQL_TABLE: &str = "ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin";

#[derive(Debug)]
pub enum Error {
    Sqlite(rusqlite::Error),
    MySql(mysql::Error),
    Other(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(err) => write!(f, "{err}"),
            Self::MySql(err) => write!(f, "{err}"),
            Self::Other(text) => write!(f, "{text}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(err: rusqlite::Error) -> Self {
        Self::Sqlite(err)
    }
}

impl From<mysql::Error> for Error {
    fn from(err: mysql::Error) -> Self {
        Self::MySql(err)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// A value going into a statement, or come out of one.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Text(String),
    Blob(Vec<u8>),
}

/// What can be a statement's parameter. By reference, so that [`params!`] takes
/// nothing from where it is used.
pub trait ToValue {
    fn to_value(&self) -> Value;
}

impl<T: ToValue + ?Sized> ToValue for &T {
    fn to_value(&self) -> Value {
        (**self).to_value()
    }
}

impl<T: ToValue> ToValue for Option<T> {
    fn to_value(&self) -> Value {
        self.as_ref().map_or(Value::Null, ToValue::to_value)
    }
}

impl ToValue for str {
    fn to_value(&self) -> Value {
        Value::Text(self.to_string())
    }
}

impl ToValue for String {
    fn to_value(&self) -> Value {
        Value::Text(self.clone())
    }
}

impl ToValue for [u8] {
    fn to_value(&self) -> Value {
        Value::Blob(self.to_vec())
    }
}

impl ToValue for Vec<u8> {
    fn to_value(&self) -> Value {
        Value::Blob(self.clone())
    }
}

impl ToValue for bool {
    fn to_value(&self) -> Value {
        Value::Int(i64::from(*self))
    }
}

impl ToValue for i64 {
    fn to_value(&self) -> Value {
        Value::Int(*self)
    }
}

impl ToValue for u32 {
    fn to_value(&self) -> Value {
        Value::Int(i64::from(*self))
    }
}

impl ToValue for Value {
    fn to_value(&self) -> Value {
        self.clone()
    }
}

/// A statement's parameters, in the order they are numbered: `params![did, now]`.
#[macro_export]
macro_rules! params {
    ($($value:expr),* $(,)?) => {
        &[$($crate::db::ToValue::to_value(&$value)),*] as &[$crate::db::Value]
    };
}

/// What a column of a row can be read as.
pub trait FromValue: Sized {
    fn from_value(value: Value) -> Option<Self>;
}

impl FromValue for i64 {
    fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::Int(number) => Some(number),
            // MySQL counts some sums in decimals, which arrive written out.
            Value::Text(text) => text.parse().ok(),
            Value::Blob(bytes) => String::from_utf8(bytes).ok()?.parse().ok(),
            Value::Null => None,
        }
    }
}

impl FromValue for u32 {
    fn from_value(value: Value) -> Option<Self> {
        i64::from_value(value).and_then(|number| number.try_into().ok())
    }
}

impl FromValue for bool {
    fn from_value(value: Value) -> Option<Self> {
        i64::from_value(value).map(|number| number != 0)
    }
}

impl FromValue for String {
    fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::Text(text) => Some(text),
            Value::Blob(bytes) => String::from_utf8(bytes).ok(),
            _ => None,
        }
    }
}

impl FromValue for Vec<u8> {
    fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::Blob(bytes) => Some(bytes),
            Value::Text(text) => Some(text.into_bytes()),
            _ => None,
        }
    }
}

impl<T: FromValue> FromValue for Option<T> {
    fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::Null => Some(None),
            value => T::from_value(value).map(Some),
        }
    }
}

/// One row of an answer.
pub struct Row(Vec<Value>);

impl Row {
    /// The column at `index`, from 0.
    pub fn get<T: FromValue>(&self, index: usize) -> Result<T> {
        let value = self.0.get(index).cloned().ok_or_else(|| Error::Other(format!("no column {index} in a row of {}", self.0.len())))?;
        T::from_value(value).ok_or_else(|| Error::Other(format!("column {index} is not what was expected of it")))
    }
}

enum Backend {
    Sqlite(rusqlite::Connection),
    MySql(Box<MySql>),
}

struct MySql {
    connection: mysql::Conn,
    options: mysql::Opts,
    used: Instant,
}

/// A service's database.
pub struct Db {
    backend: Mutex<Backend>,
}

/// The database, held: nothing else is said to it until this is dropped.
pub struct Session<'a> {
    backend: MutexGuard<'a, Backend>,
    in_transaction: bool,
}

/// How long a MySQL connection is left before it is asked whether it is still there.
const CHECK_AFTER: Duration = Duration::from_secs(30);

impl Db {
    /// Opens the store at `location` and makes `schema`'s tables in it. `service`
    /// is the name the store is marked with: a MySQL database is one service's,
    /// and another's is refused rather than written into.
    pub fn open(location: &Location, schema: &Schema, service: &str) -> Result<Self> {
        match location {
            Location::Sqlite(path) => {
                if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent).map_err(|err| Error::Other(format!("making {}: {err}", parent.display())))?;
                }
                Self::sqlite(rusqlite::Connection::open(path)?, schema)
            }
            Location::MySql(url) => Self::mysql(mysql_options(url)?, schema, service),
        }
    }

    /// A store that lasts as long as it is held: for tests. In memory, unless
    /// `WYVRN_TEST_DATABASE_URL` names a MySQL server: then a database of its own
    /// there, made for it and left behind, so that the same tests can be run
    /// against the real thing.
    pub fn scratch(schema: &Schema) -> Result<Self> {
        match std::env::var("WYVRN_TEST_DATABASE_URL").ok().filter(|url| !url.is_empty()) {
            None => Self::sqlite(rusqlite::Connection::open_in_memory()?, schema),
            Some(url) => {
                let Location::MySql(url) = Location::parse(&url)? else {
                    return Err(Error::Other("WYVRN_TEST_DATABASE_URL is for a MySQL server".into()));
                };
                let server = mysql_options(&url)?;
                let name = format!("wyvrn_test_{}", scratch_name());
                let mut connection = mysql::Conn::new(server.clone())?;
                connection.query_drop(format!("CREATE DATABASE {name}"))?;
                let options = mysql::OptsBuilder::from_opts(server).db_name(Some(name));
                Self::mysql(options.into(), schema, "test")
            }
        }
    }

    fn sqlite(connection: rusqlite::Connection, schema: &Schema) -> Result<Self> {
        // A service's own commands (`members`, `add`, …) open the same file while
        // the server has it open.
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(schema.sqlite)?;
        Ok(Self { backend: Mutex::new(Backend::Sqlite(connection)) })
    }

    fn mysql(options: mysql::Opts, schema: &Schema, service: &str) -> Result<Self> {
        let mut connection = mysql_connection(&options)?;
        for statement in schema.mysql.split(';').map(str::trim).filter(|statement| !statement.is_empty()) {
            connection.query_drop(statement)?;
        }
        // Whose database this is. Two services' tables share names, and not
        // columns: one started on another's would fail at its first question.
        connection.query_drop(format!("CREATE TABLE IF NOT EXISTS wyvrn_service (service VARCHAR(64) NOT NULL PRIMARY KEY) {MYSQL_TABLE}"))?;
        let owner: Option<String> = connection.query_first("SELECT service FROM wyvrn_service LIMIT 1")?;
        match owner {
            None => connection.exec_drop("INSERT INTO wyvrn_service (service) VALUES (?)", (service,))?,
            Some(owner) if owner == service => {}
            Some(owner) => {
                return Err(Error::Other(format!(
                    "this database is {owner}'s, and this is {service}: each service needs a database of its own (a different name at the end of its DATABASE_URL)"
                )))
            }
        }
        Ok(Self { backend: Mutex::new(Backend::MySql(Box::new(MySql { connection, options, used: Instant::now() }))) })
    }

    /// Holds the database: what is said through the session is said with nothing
    /// from anywhere else in this process between.
    pub fn lock(&self) -> Session<'_> {
        Session { backend: self.backend.lock().unwrap_or_else(|poisoned| poisoned.into_inner()), in_transaction: false }
    }
}

fn scratch_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("{}_{}_{}", std::process::id(), nanos, NEXT.fetch_add(1, Ordering::Relaxed))
}

fn mysql_options(url: &str) -> Result<mysql::Opts> {
    let options = mysql::Opts::from_url(url).map_err(|err| Error::Other(format!("the database URL can't be read: {err}")))?;
    // An UPDATE says how many rows it found, not how many it changed: as SQLite's
    // does, and as the stores read it ("was there one?").
    Ok(mysql::OptsBuilder::from_opts(options).additional_capabilities(mysql::consts::CapabilityFlags::CLIENT_FOUND_ROWS).into())
}

fn mysql_connection(options: &mysql::Opts) -> Result<mysql::Conn> {
    let mut connection = mysql::Conn::new(options.clone())?;
    // `||` joins text and "…" quotes a name, as in SQLite; and a value that doesn't
    // fit its column is refused, not cut short.
    connection.query_drop("SET SESSION sql_mode = 'STRICT_ALL_TABLES,PIPES_AS_CONCAT,ANSI_QUOTES,NO_ENGINE_SUBSTITUTION'")?;
    connection.query_drop("SET NAMES utf8mb4 COLLATE utf8mb4_bin")?;
    Ok(connection)
}

/// A statement as MySQL takes it, and which parameter goes in each of its places.
///
/// The parameters lose their numbers (`?2` used twice is two places given the same
/// value), and SQLite's "or" clauses become MySQL's words for the same things.
fn for_mysql(sql: &str) -> (String, Vec<usize>) {
    let sql = sql.replace("INSERT OR IGNORE", "INSERT IGNORE").replace("INSERT OR REPLACE", "REPLACE").replace("UPDATE OR IGNORE", "UPDATE IGNORE");
    let mut said = String::with_capacity(sql.len());
    let mut places = Vec::new();
    let mut characters = sql.chars().peekable();
    let mut quoted: Option<char> = None;
    while let Some(character) = characters.next() {
        said.push(character);
        match quoted {
            Some(quote) if character == quote => quoted = None,
            Some(_) => {}
            None if character == '\'' || character == '"' => quoted = Some(character),
            None if character == '?' => {
                let mut number = String::new();
                while let Some(digit) = characters.peek().filter(|c| c.is_ascii_digit()) {
                    number.push(*digit);
                    characters.next();
                }
                places.push(number.parse::<usize>().map_or(places.len(), |n| n.saturating_sub(1)));
            }
            None => {}
        }
    }
    (said, places)
}

impl rusqlite::ToSql for Value {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        use rusqlite::types::{ToSqlOutput, ValueRef};
        Ok(ToSqlOutput::Borrowed(match self {
            Value::Null => ValueRef::Null,
            Value::Int(number) => ValueRef::Integer(*number),
            Value::Text(text) => ValueRef::Text(text.as_bytes()),
            Value::Blob(bytes) => ValueRef::Blob(bytes),
        }))
    }
}

fn from_sqlite(row: &rusqlite::Row<'_>, columns: usize) -> rusqlite::Result<Row> {
    use rusqlite::types::ValueRef;
    (0..columns)
        .map(|index| {
            Ok(match row.get_ref(index)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(number) => Value::Int(number),
                ValueRef::Real(number) => Value::Int(number as i64),
                ValueRef::Text(text) => Value::Text(String::from_utf8_lossy(text).into_owned()),
                ValueRef::Blob(bytes) => Value::Blob(bytes.to_vec()),
            })
        })
        .collect::<rusqlite::Result<Vec<_>>>()
        .map(Row)
}

fn from_mysql(row: mysql::Row) -> Row {
    Row(row
        .unwrap()
        .into_iter()
        .map(|value| match value {
            mysql::Value::NULL => Value::Null,
            mysql::Value::Int(number) => Value::Int(number),
            mysql::Value::UInt(number) => Value::Int(number as i64),
            mysql::Value::Float(number) => Value::Int(number as i64),
            mysql::Value::Double(number) => Value::Int(number as i64),
            // Text and bytes arrive alike; what reads the column says which it wants.
            mysql::Value::Bytes(bytes) => Value::Blob(bytes),
            other => Value::Text(other.as_sql(true)),
        })
        .collect())
}

fn to_mysql(sql: &str, params: &[Value]) -> Result<(String, mysql::Params)> {
    let (said, places) = for_mysql(sql);
    if places.is_empty() {
        return Ok((said, mysql::Params::Empty));
    }
    let values = places
        .iter()
        .map(|&place| {
            Ok(match params.get(place).ok_or_else(|| Error::Other(format!("a statement wants parameter {} and was given {}", place + 1, params.len())))? {
                Value::Null => mysql::Value::NULL,
                Value::Int(number) => mysql::Value::Int(*number),
                Value::Text(text) => mysql::Value::Bytes(text.clone().into_bytes()),
                Value::Blob(bytes) => mysql::Value::Bytes(bytes.clone()),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((said, mysql::Params::Positional(values)))
}

/// Runs something that waits on the network without holding up the rest of the
/// service, where there is a rest of it to hold up.
fn waiting<T>(work: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => tokio::task::block_in_place(work),
        _ => work(),
    }
}

/// What a statement did, and what it answered with.
struct Outcome {
    affected: usize,
    inserted: i64,
    rows: Vec<Row>,
}

impl MySql {
    /// The connection, known to be there: one left a while is asked, and made
    /// again if it has gone (a server restarted, an idle connection closed).
    fn ready(&mut self, in_transaction: bool) -> Result<()> {
        if !in_transaction && self.used.elapsed() > CHECK_AFTER && self.connection.ping().is_err() {
            self.connection = mysql_connection(&self.options)?;
        }
        self.used = Instant::now();
        Ok(())
    }

    fn say(&mut self, sql: &str, params: &[Value]) -> Result<Outcome> {
        let (said, values) = to_mysql(sql, params)?;
        let mut answer = self.connection.exec_iter(said, values)?;
        let affected = answer.affected_rows() as usize;
        let inserted = answer.last_insert_id().unwrap_or(0) as i64;
        let mut rows = Vec::new();
        for row in answer.by_ref() {
            rows.push(from_mysql(row?));
        }
        Ok(Outcome { affected, inserted, rows })
    }
}

impl Session<'_> {
    fn say(&mut self, sql: &str, params: &[Value]) -> Result<Outcome> {
        let in_transaction = self.in_transaction;
        match &mut *self.backend {
            Backend::Sqlite(connection) => {
                let mut statement = connection.prepare_cached(sql)?;
                let columns = statement.column_count();
                if columns == 0 {
                    let affected = statement.execute(rusqlite::params_from_iter(params))?;
                    drop(statement);
                    return Ok(Outcome { affected, inserted: connection.last_insert_rowid(), rows: Vec::new() });
                }
                let rows = statement.query_map(rusqlite::params_from_iter(params), |row| from_sqlite(row, columns))?.collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(Outcome { affected: 0, inserted: 0, rows })
            }
            Backend::MySql(mysql) => waiting(|| {
                mysql.ready(in_transaction)?;
                match mysql.say(sql, params) {
                    // The connection went (the server restarted, or closed it): said
                    // again on a new one. Not part-way through a transaction, whose
                    // earlier statements went with it.
                    Err(Error::MySql(mysql::Error::IoError(_) | mysql::Error::CodecError(_))) if !in_transaction => {
                        mysql.connection = mysql_connection(&mysql.options)?;
                        mysql.say(sql, params)
                    }
                    answer => answer,
                }
            }),
        }
    }

    /// Runs a statement that answers with nothing. Returns how many rows it
    /// touched: for an `UPDATE`, how many it found.
    pub fn execute(&mut self, sql: &str, params: &[Value]) -> Result<usize> {
        Ok(self.say(sql, params)?.affected)
    }

    /// Runs an `INSERT` into a table that numbers its rows, and returns the number
    /// the new row was given.
    pub fn insert(&mut self, sql: &str, params: &[Value]) -> Result<i64> {
        Ok(self.say(sql, params)?.inserted)
    }

    /// Every row a query answers with, each read by `read`.
    pub fn query<T>(&mut self, sql: &str, params: &[Value], read: impl Fn(&Row) -> Result<T>) -> Result<Vec<T>> {
        self.say(sql, params)?.rows.iter().map(read).collect()
    }

    /// The first row a query answers with, if it answers with any.
    pub fn query_row<T>(&mut self, sql: &str, params: &[Value], read: impl Fn(&Row) -> Result<T>) -> Result<Option<T>> {
        self.say(sql, params)?.rows.first().map(read).transpose()
    }

    /// The first column of the one row a query always answers with (a count, a
    /// maximum).
    pub fn query_value<T: FromValue>(&mut self, sql: &str, params: &[Value]) -> Result<T> {
        self.query_row(sql, params, |row| row.get(0))?.ok_or_else(|| Error::Other("a query that always answers answered with nothing".into()))
    }

    /// Does all of `work` or none of it: what it changed is undone if it fails.
    pub fn transaction<T, E: From<Error>>(&mut self, work: impl FnOnce(&mut Self) -> std::result::Result<T, E>) -> std::result::Result<T, E> {
        self.plainly(match &*self.backend {
            Backend::Sqlite(_) => "BEGIN IMMEDIATE",
            Backend::MySql(_) => "START TRANSACTION",
        })?;
        self.in_transaction = true;
        let done = work(self);
        self.in_transaction = false;
        match done {
            Ok(value) => {
                self.plainly("COMMIT")?;
                Ok(value)
            }
            Err(err) => {
                let _ = self.plainly("ROLLBACK");
                Err(err)
            }
        }
    }

    /// Says a statement with no parameters and no answer, as it is written.
    fn plainly(&mut self, sql: &str) -> Result<()> {
        let in_transaction = self.in_transaction;
        match &mut *self.backend {
            Backend::Sqlite(connection) => connection.execute_batch(sql)?,
            Backend::MySql(mysql) => waiting(|| {
                mysql.ready(in_transaction)?;
                Ok::<_, Error>(mysql.connection.query_drop(sql)?)
            })?,
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The MySQL server the tests were given, if they were given one.
    fn server() -> Option<String> {
        std::env::var("WYVRN_TEST_DATABASE_URL").ok().filter(|url| !url.is_empty())
    }

    const SCHEMA: Schema = Schema {
        sqlite: "CREATE TABLE IF NOT EXISTS things (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, size INTEGER NOT NULL, bytes BLOB, note TEXT)",
        mysql: "CREATE TABLE IF NOT EXISTS things (id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY, name VARCHAR(255) NOT NULL UNIQUE, size BIGINT NOT NULL, bytes LONGBLOB, note TEXT) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin",
    };

    #[test]
    fn a_statement_for_mysql_loses_its_numbers_and_keeps_their_order() {
        let (said, places) = for_mysql("UPDATE t SET a = ?2 WHERE b = ?1 AND c < ?2 AND d = '?3'");
        assert_eq!(said, "UPDATE t SET a = ? WHERE b = ? AND c < ? AND d = '?3'");
        assert_eq!(places, [1, 0, 1]);
        assert_eq!(for_mysql("INSERT OR IGNORE INTO t (a) VALUES (?1)").0, "INSERT IGNORE INTO t (a) VALUES (?)");
        assert_eq!(for_mysql("INSERT OR REPLACE INTO t (a) VALUES (?1)").0, "REPLACE INTO t (a) VALUES (?)");
    }

    #[test]
    fn a_url_says_where_and_is_printed_without_its_password() {
        assert_eq!(Location::parse("sqlite://data/x.sqlite").unwrap(), Location::Sqlite("data/x.sqlite".into()));
        let mysql = Location::parse("mariadb://wyvrn:hunter2@db:3306/community").unwrap();
        assert_eq!(mysql, Location::MySql("mysql://wyvrn:hunter2@db:3306/community".into()));
        assert_eq!(mysql.to_string(), "mysql://…@db:3306/community");
        assert!(Location::parse("postgres://x").is_err());
    }

    #[test]
    fn what_is_written_is_read_back_and_a_failed_transaction_leaves_nothing() {
        let db = Db::scratch(&SCHEMA).unwrap();
        let mut session = db.lock();
        let first = session.insert("INSERT INTO things (name, size, bytes, note) VALUES (?1, ?2, ?3, ?4)", params!["one", 1i64, [0u8, 255, 7].as_slice(), None::<String>]).unwrap();
        let second = session.insert("INSERT INTO things (name, size) VALUES (?1, ?2)", params!["Two — ünïcode 🐉", 2i64]).unwrap();
        assert_eq!((first, second), (1, 2));
        let read = session
            .query("SELECT name, size, bytes, note FROM things ORDER BY id", params![], |row| {
                Ok((row.get::<String>(0)?, row.get::<i64>(1)?, row.get::<Option<Vec<u8>>>(2)?, row.get::<Option<String>>(3)?))
            })
            .unwrap();
        assert_eq!(read[0], ("one".to_string(), 1, Some(vec![0, 255, 7]), None));
        assert_eq!(read[1].0, "Two — ünïcode 🐉");
        // Names are told apart by case, and an update says what it found.
        assert_eq!(session.query_value::<i64>("SELECT COUNT(*) FROM things WHERE name = ?1", params!["ONE"]).unwrap(), 0);
        assert_eq!(session.execute("UPDATE things SET size = ?2 WHERE name = ?1", params!["one", 1i64]).unwrap(), 1);
        assert_eq!(session.execute("INSERT OR IGNORE INTO things (name, size) VALUES (?1, ?2)", params!["one", 9i64]).unwrap(), 0);
        assert_eq!(session.query_value::<String>("SELECT 'a' || name FROM things WHERE id = ?1", params![1i64]).unwrap(), "aone");

        let failed: Result<()> = session.transaction(|session| {
            session.execute("UPDATE things SET size = 100 WHERE id = 1", params![])?;
            Err(Error::Other("no".into()))
        });
        assert!(failed.is_err());
        assert_eq!(session.query_value::<i64>("SELECT size FROM things WHERE id = 1", params![]).unwrap(), 1);
        assert!(session.query_row("SELECT name FROM things WHERE id = ?1", params![99i64], |row| row.get::<String>(0)).unwrap().is_none());
    }

    #[test]
    fn a_did_longer_than_most_keys_is_one_and_is_told_from_another_to_its_last_character() {
        const DIDS: Schema = Schema {
            sqlite: "CREATE TABLE IF NOT EXISTS dids (did TEXT PRIMARY KEY, n INTEGER NOT NULL)",
            mysql: "CREATE TABLE IF NOT EXISTS dids (did VARCHAR(3000) CHARACTER SET ascii COLLATE ascii_bin NOT NULL PRIMARY KEY, n BIGINT NOT NULL) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin",
        };
        let db = Db::scratch(&DIDS).unwrap();
        let mut session = db.lock();
        let long = format!("did:peer:4zQm{}", "a1B2".repeat(700));
        session.execute("INSERT INTO dids (did, n) VALUES (?1, 1)", params![long]).unwrap();
        session.execute("INSERT INTO dids (did, n) VALUES (?1, 2)", params![format!("{long}x")]).unwrap();
        assert!(session.execute("INSERT INTO dids (did, n) VALUES (?1, 3)", params![long]).is_err());
        assert_eq!(session.query_value::<i64>("SELECT n FROM dids WHERE did = ?1", params![long]).unwrap(), 1);
        assert_eq!(session.query_value::<i64>("SELECT COUNT(*) FROM dids WHERE did = ?1", params![long.to_uppercase()]).unwrap(), 0);
    }

    #[test]
    fn a_mysql_database_is_one_services_and_a_lost_connection_is_made_again() {
        let Some(url) = server() else { return };
        let Location::MySql(url) = Location::parse(&url).unwrap() else { panic!("WYVRN_TEST_DATABASE_URL is for a MySQL server") };
        let name = format!("wyvrn_test_{}", scratch_name());
        let mut root = mysql::Conn::new(mysql_options(&url).unwrap()).unwrap();
        root.query_drop(format!("CREATE DATABASE {name}")).unwrap();
        let options: mysql::Opts = mysql::OptsBuilder::from_opts(mysql_options(&url).unwrap()).db_name(Some(name)).into();

        let db = Db::mysql(options.clone(), &SCHEMA, "community").unwrap();
        // Opened again by the same service, as its own commands do; refused to another.
        assert!(Db::mysql(options.clone(), &SCHEMA, "community").is_ok());
        let refused = Db::mysql(options, &SCHEMA, "stickers").err().expect("another service's database").to_string();
        assert!(refused.contains("community") && refused.contains("stickers"), "{refused}");

        let mut session = db.lock();
        session.execute("INSERT INTO things (name, size) VALUES (?1, ?2)", params!["kept", 1i64]).unwrap();
        let connection: i64 = session.query_value("SELECT CONNECTION_ID()", params![]).unwrap();
        root.query_drop(format!("KILL {connection}")).unwrap();
        assert_eq!(session.query_value::<i64>("SELECT size FROM things WHERE name = ?1", params!["kept"]).unwrap(), 1);
        assert_ne!(session.query_value::<i64>("SELECT CONNECTION_ID()", params![]).unwrap(), connection);
    }
}
