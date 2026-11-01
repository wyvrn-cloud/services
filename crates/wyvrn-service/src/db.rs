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
//!
//! **Many stores in one database.** A host of many communities keeps each one's
//! stores apart by naming its tables ([`Location::Shared`]): a database of a MySQL
//! server and a prefix, `"<database>"."<prefix><table>"`, written into each
//! statement as it is said ([`renamed`]). So a statement names a table only where
//! that can find it: after `FROM`, `INTO`, `JOIN`, `UPDATE [OR IGNORE]`, `TABLE [IF
//! NOT EXISTS]` and an index's `ON`, never qualifying a column with its table. Such
//! stores share the server's connections ([`pool`]): one each would be thousands.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use mysql::prelude::Queryable;

/// Where a store is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Sqlite(PathBuf),
    /// `mysql://user:password@host:port/database`.
    MySql(String),
    /// Tables named `"<database>"."<prefix><table>"` on the MySQL server at
    /// `server` (`mysql://user:password@host:port`, no database), whose connections
    /// are shared with every other store there: one of many communities kept by one
    /// host. The database is made if it isn't there. Both names are letters, digits
    /// and `_`; the prefix may be empty.
    Shared { server: String, database: String, prefix: String },
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
            Self::Shared { server, database, prefix } => write!(f, "{} {database} ({prefix}…)", Self::without_password(server)),
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
    Pooled(mysql::Pool),
}

struct MySql {
    connection: mysql::Conn,
    options: mysql::Opts,
    used: Instant,
}

/// A service's database.
pub struct Db {
    backend: Mutex<Backend>,
    naming: Option<Naming>,
}

/// What a store's tables are called where it shares a database (see
/// [`Location::Shared`]): `None`, as they are written.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Naming {
    /// The database, said with each table, where connections are shared.
    database: Option<String>,
    prefix: String,
}

/// The database, held: nothing else is said to it until this is dropped.
pub struct Session<'a> {
    backend: MutexGuard<'a, Backend>,
    naming: Option<&'a Naming>,
    /// A shared connection, taken at the first statement and given back with the session.
    pooled: Option<mysql::PooledConn>,
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
                Self::sqlite(rusqlite::Connection::open(path)?, schema, None)
            }
            Location::MySql(url) => Self::mysql(mysql_options(url)?, schema, service),
            Location::Shared { server, database, prefix } => Self::shared(server, database, prefix, schema, service),
        }
    }

    /// A store that lasts as long as it is held: for tests. In memory, unless
    /// `WYVRN_TEST_DATABASE_URL` names a MySQL server: then a database of its own
    /// there, made for it and left behind, so that the same tests can be run
    /// against the real thing.
    ///
    /// `WYVRN_TEST_TABLE_PREFIX` names its tables with that prefix, as a host of
    /// many communities does (on MySQL, in a database shared by the tests that
    /// run at once): every store's tests run so find a statement that names a
    /// table where the prefix can't be written in.
    pub fn scratch(schema: &Schema) -> Result<Self> {
        let prefix = std::env::var("WYVRN_TEST_TABLE_PREFIX").ok().filter(|prefix| !prefix.is_empty());
        match std::env::var("WYVRN_TEST_DATABASE_URL").ok().filter(|url| !url.is_empty()) {
            None => {
                let naming = prefix.map(|prefix| Naming { database: None, prefix: format!("{prefix}{}_", scratch_name()) });
                Self::sqlite(rusqlite::Connection::open_in_memory()?, schema, naming)
            }
            Some(url) => {
                let Location::MySql(url) = Location::parse(&url)? else {
                    return Err(Error::Other("WYVRN_TEST_DATABASE_URL is for a MySQL server".into()));
                };
                if let Some(prefix) = prefix {
                    let server = mysql_options(&url)?;
                    let server = mysql::OptsBuilder::from_opts(server).db_name(None::<String>);
                    return Self::shared_with(server.into(), "wyvrn_test_shared", &format!("{prefix}{}_", scratch_name()), schema, "test");
                }
                let server = mysql_options(&url)?;
                let name = format!("wyvrn_test_{}", scratch_name());
                let mut connection = mysql::Conn::new(server.clone())?;
                connection.query_drop(format!("CREATE DATABASE {name}"))?;
                let options = mysql::OptsBuilder::from_opts(server).db_name(Some(name));
                Self::mysql(options.into(), schema, "test")
            }
        }
    }

    fn sqlite(connection: rusqlite::Connection, schema: &Schema, naming: Option<Naming>) -> Result<Self> {
        // A service's own commands (`members`, `add`, …) open the same file while
        // the server has it open.
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.busy_timeout(Duration::from_secs(5))?;
        match &naming {
            None => connection.execute_batch(schema.sqlite)?,
            Some(naming) => connection.execute_batch(&renamed(schema.sqlite, naming))?,
        }
        Ok(Self { backend: Mutex::new(Backend::Sqlite(connection)), naming })
    }

    fn mysql(options: mysql::Opts, schema: &Schema, service: &str) -> Result<Self> {
        let connection = mysql_connection(&options)?;
        let db = Self { backend: Mutex::new(Backend::MySql(Box::new(MySql { connection, options, used: Instant::now() }))), naming: None };
        db.made(schema, service)?;
        Ok(db)
    }

    fn shared(server: &str, database: &str, prefix: &str, schema: &Schema, service: &str) -> Result<Self> {
        let options = mysql_options(server)?;
        if options.get_db_name().is_some() {
            return Err(Error::Other(format!("a shared server's URL names no database, and this one does ({})", Location::without_password(server))));
        }
        Self::shared_with(options, database, prefix, schema, service)
    }

    fn shared_with(server: mysql::Opts, database: &str, prefix: &str, schema: &Schema, service: &str) -> Result<Self> {
        let plain = |name: &str| name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if database.is_empty() || !plain(database) || !plain(prefix) {
            return Err(Error::Other(format!("a database and a table prefix are letters, digits and _ ({database:?}, {prefix:?})")));
        }
        let pool = pool(server)?;
        pool.get_conn()?.query_drop(format!("CREATE DATABASE IF NOT EXISTS \"{database}\""))?;
        let naming = Naming { database: Some(database.to_string()), prefix: prefix.to_string() };
        let db = Self { backend: Mutex::new(Backend::Pooled(pool)), naming: Some(naming) };
        db.made(schema, service)?;
        Ok(db)
    }

    /// Makes a MySQL store's tables, and marks it as `service`'s: two services'
    /// tables share names, and not columns, so one started on another's would fail
    /// at its first question.
    fn made(&self, schema: &Schema, service: &str) -> Result<()> {
        let mut session = self.lock();
        for statement in schema.mysql.split(';').map(str::trim).filter(|statement| !statement.is_empty()) {
            session.plainly(statement)?;
        }
        session.plainly(&format!("CREATE TABLE IF NOT EXISTS wyvrn_service (service VARCHAR(64) NOT NULL PRIMARY KEY) {MYSQL_TABLE}"))?;
        let owner: Option<String> = session.query_row("SELECT service FROM wyvrn_service LIMIT 1", &[], |row| row.get(0))?;
        match owner {
            None => {
                session.execute("INSERT INTO wyvrn_service (service) VALUES (?1)", params![service])?;
            }
            Some(owner) if owner == service => {}
            Some(owner) => {
                return Err(Error::Other(format!(
                    "this database is {owner}'s, and this is {service}: each service needs a database of its own (a different name at the end of its DATABASE_URL), or tables of its own (a different prefix)"
                )))
            }
        }
        Ok(())
    }

    /// Holds the database: what is said through the session is said with nothing
    /// from anywhere else in this process between.
    pub fn lock(&self) -> Session<'_> {
        Session {
            backend: self.backend.lock().unwrap_or_else(|poisoned| poisoned.into_inner()),
            naming: self.naming.as_ref(),
            pooled: None,
            in_transaction: false,
        }
    }
}

/// The connections to the MySQL server `server` names, shared by every store
/// there that shares them ([`Location::Shared`]). At most
/// `WYVRN_MYSQL_CONNECTIONS` (64) at once; a store waits for one past that.
fn pool(server: mysql::Opts) -> Result<mysql::Pool> {
    static POOLS: OnceLock<Mutex<HashMap<String, mysql::Pool>>> = OnceLock::new();
    let mut pools = POOLS.get_or_init(Default::default).lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // (Known by everything in it: `Opts` can't be a key itself. Never printed.)
    let key = format!("{server:?}");
    if let Some(pool) = pools.get(&key) {
        return Ok(pool.clone());
    }
    let most = std::env::var("WYVRN_MYSQL_CONNECTIONS").ok().and_then(|n| n.parse().ok()).filter(|&n: &usize| n > 0).unwrap_or(64);
    let constraints = mysql::PoolConstraints::new(0, most).ok_or_else(|| Error::Other("WYVRN_MYSQL_CONNECTIONS can't be used".into()))?;
    // Nothing a store does outlives its session (a transaction is ended in it), so
    // a connection goes back as it is: resetting it would undo what `init` set.
    // Not asked whether it is there each time it is taken: one that has gone is
    // found by the statement, and said again on another (Session::say).
    let options = mysql::OptsBuilder::from_opts(server)
        .init(SESSION.to_vec())
        .pool_opts(mysql::PoolOpts::default().with_constraints(constraints).with_reset_connection(false).with_check_health(false));
    let pool = mysql::Pool::new(options)?;
    pools.insert(key, pool.clone());
    Ok(pool)
}

/// What [`copy_store`] copied: each table, and how many rows.
pub type Copied = Vec<(String, usize)>;

/// Copies the store at `from` into the same store at `to`, whose tables have to
/// be made already (by opening it as its service does) and be empty: every table
/// of `from`'s own but those in `skip`, row for row. `service` is whose store it is.
/// Text stays text and bytes bytes between kinds of database, by what each column
/// is. One transaction at `to`: a copy that fails leaves nothing there.
pub fn copy_store(from: &Location, to: &Location, service: &str, skip: &[&str]) -> Result<Copied> {
    const NOTHING: Schema = Schema { sqlite: "", mysql: "" };
    let (source, target) = (Db::open(from, &NOTHING, service)?, Db::open(to, &NOTHING, service)?);
    let mut reading = source.lock();
    let mut writing = target.lock();
    let tables: Vec<String> = reading.tables()?.into_iter().filter(|table| !skip.contains(&table.as_str())).collect();
    writing.transaction(|writing| {
        let mut copied = Vec::new();
        for table in &tables {
            let quoted = format!("\"{table}\"");
            let held: i64 = writing.query_value(&format!("SELECT COUNT(*) FROM {quoted}"), &[])?;
            if held > 0 {
                return Err(Error::Other(format!("{table} at {to} isn't empty: a store is copied only into one that has nothing")));
            }
            let (columns, rows) = reading.dump(table)?;
            if !columns.is_empty() {
                let names = columns.iter().map(|column| format!("\"{column}\"")).collect::<Vec<_>>().join(", ");
                let places = (1..=columns.len()).map(|n| format!("?{n}")).collect::<Vec<_>>().join(", ");
                let insert = format!("INSERT INTO {quoted} ({names}) VALUES ({places})");
                for row in &rows {
                    writing.execute(&insert, row)?;
                }
            }
            copied.push((table.clone(), rows.len()));
        }
        Ok(copied)
    })
}

/// Said on every new MySQL connection. `||` joins text and "…" quotes a name, as
/// in SQLite; and a value that doesn't fit its column is refused, not cut short.
const SESSION: [&str; 2] = ["SET SESSION sql_mode = 'STRICT_ALL_TABLES,PIPES_AS_CONCAT,ANSI_QUOTES,NO_ENGINE_SUBSTITUTION'", "SET NAMES utf8mb4 COLLATE utf8mb4_bin"];

/// `sql` with every table it names called as `naming` says: `posts` becomes
/// `"c12_posts"`, or `"db"."c12_posts"` where the database is said too. A table
/// is the name after `FROM`, `INTO`, `JOIN`, `UPDATE` and `TABLE` (past `OR
/// IGNORE` and `IF NOT EXISTS`), and after `ON` in `CREATE INDEX`, whose own name
/// is given the prefix (an index's name is the database's in SQLite). A table a
/// statement qualifies its columns with (`reactions.emoji`) and gives no other
/// name keeps its own as one (`"c12_reactions" AS reactions`). Strings and `--`
/// comments are left as they are. Keywords are recognised in capitals only, as
/// every statement here is written.
fn renamed(sql: &str, naming: &Naming) -> String {
    /// Whether `sql` names a column `table.column`.
    fn qualifies(sql: &str, table: &str) -> bool {
        let wanted = format!("{table}.");
        sql.match_indices(&wanted).any(|(at, _)| !sql[..at].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '"'))
    }
    /// Whether what follows a table gives it a name of its own.
    fn aliased(rest: &str) -> bool {
        const NOT_A_NAME: &[&str] = &[
            "WHERE", "ORDER", "GROUP", "LIMIT", "OFFSET", "LEFT", "RIGHT", "INNER", "CROSS", "OUTER", "NATURAL", "JOIN", "ON", "USING", "UNION", "EXCEPT",
            "INTERSECT", "HAVING", "WINDOW", "SET", "VALUES", "SELECT", "DEFAULT",
        ];
        let rest = rest.trim_start();
        let word: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        !word.is_empty() && !NOT_A_NAME.contains(&word.as_str())
    }
    #[derive(PartialEq)]
    enum Next {
        Nothing,
        Table,
        IndexName,
        IndexOn,
    }
    let table = |name: &str| match &naming.database {
        Some(database) => format!("\"{database}\".\"{}{name}\"", naming.prefix),
        None => format!("\"{}{name}\"", naming.prefix),
    };
    let mut said = String::with_capacity(sql.len() + 64);
    let mut next = Next::Nothing;
    let bytes = sql.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        let c = bytes[at];
        if c == b'\'' {
            let end = sql[at + 1..].find('\'').map_or(sql.len(), |i| at + 1 + i + 1);
            said.push_str(&sql[at..end]);
            at = end;
        } else if c == b'-' && bytes.get(at + 1) == Some(&b'-') {
            let end = sql[at..].find('\n').map_or(sql.len(), |i| at + i);
            said.push_str(&sql[at..end]);
            at = end;
        } else if c == b'"' || c.is_ascii_alphanumeric() || c == b'_' {
            let (end, name) = if c == b'"' {
                let end = sql[at + 1..].find('"').map_or(sql.len(), |i| at + 1 + i + 1);
                (end, sql[at + 1..end.saturating_sub(1).max(at + 1)].to_string())
            } else {
                let end = sql[at..].find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).map_or(sql.len(), |i| at + i);
                (end, sql[at..end].to_string())
            };
            let word = &sql[at..end];
            match (c == b'"', word) {
                (false, "FROM" | "INTO" | "JOIN" | "UPDATE" | "TABLE") => {
                    said.push_str(word);
                    next = Next::Table;
                }
                (false, "INDEX") => {
                    said.push_str(word);
                    next = Next::IndexName;
                }
                (false, "ON") if next == Next::IndexOn => {
                    said.push_str(word);
                    next = Next::Table;
                }
                (false, "OR" | "IGNORE" | "IF" | "NOT" | "EXISTS") if matches!(next, Next::Table | Next::IndexName) => said.push_str(word),
                _ if next == Next::Table => {
                    said.push_str(&table(&name));
                    if qualifies(sql, &name) && !aliased(&sql[end..]) {
                        said.push_str(" AS ");
                        said.push_str(&name);
                    }
                    next = Next::Nothing;
                }
                _ if next == Next::IndexName => {
                    said.push_str(&format!("\"{}{name}\"", naming.prefix));
                    next = Next::IndexOn;
                }
                _ => said.push_str(word),
            }
            at = end;
        } else {
            if !c.is_ascii_whitespace() && next == Next::Table || c == b'(' && next == Next::IndexName || c == b';' {
                // A subquery, `INDEX (…)` in a MySQL table, or the statement's end.
                next = Next::Nothing;
            }
            let end = at + sql[at..].chars().next().map_or(1, char::len_utf8);
            said.push_str(&sql[at..end]);
            at = end;
        }
    }
    said
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
    for statement in SESSION {
        connection.query_drop(statement)?;
    }
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

/// Says a statement on a MySQL connection.
fn say_on(connection: &mut impl Queryable, sql: &str, params: &[Value]) -> Result<Outcome> {
    let (said, values) = to_mysql(sql, params)?;
    let mut answer = connection.exec_iter(said, values)?;
    let affected = answer.affected_rows() as usize;
    let inserted = answer.last_insert_id().unwrap_or(0) as i64;
    let mut rows = Vec::new();
    for row in answer.by_ref() {
        rows.push(from_mysql(row?));
    }
    Ok(Outcome { affected, inserted, rows })
}

/// Reading a whole table off a MySQL connection, pooled or not.
trait MySqlReading {
    fn dump(&mut self, sql: &str) -> Result<(Vec<String>, Vec<Vec<Value>>)>;
}

impl<T: Queryable> MySqlReading for T {
    fn dump(&mut self, sql: &str) -> Result<(Vec<String>, Vec<Vec<Value>>)> {
        // The binary protocol: numbers come as numbers, not written out.
        let mut answer = self.exec_iter(sql, ())?;
        let columns = answer.columns();
        let names: Vec<String> = columns.as_ref().iter().map(|column| column.name_str().into_owned()).collect();
        // Text and bytes arrive alike; a column whose character set is binary's (63)
        // holds bytes, any other text.
        let text: Vec<bool> = columns.as_ref().iter().map(|column| column.character_set() != 63).collect();
        let mut rows = Vec::new();
        for row in answer.by_ref() {
            let Row(values) = from_mysql(row?);
            rows.push(
                values
                    .into_iter()
                    .zip(&text)
                    .map(|(value, &text)| match value {
                        Value::Blob(bytes) if text => String::from_utf8(bytes).map_or_else(|bytes| Value::Blob(bytes.into_bytes()), Value::Text),
                        other => other,
                    })
                    .collect(),
            );
        }
        Ok((names, rows))
    }
}

/// Whether a MySQL error says the connection has gone, so that the statement can
/// be said again on another.
fn gone(error: &Error) -> bool {
    matches!(error, Error::MySql(mysql::Error::IoError(_) | mysql::Error::CodecError(_)))
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
        say_on(&mut self.connection, sql, params)
    }
}

/// How many times a statement is said on another shared connection when the one
/// it was said on has gone: after a server restarts, every one kept has.
const TRIES: usize = 4;

impl Session<'_> {
    /// Says `work` on the shared connection this session holds, taking one first. A
    /// connection that has gone is let go of (not given back) and, outside a
    /// transaction, the work is done again on another.
    fn on_shared<T>(&mut self, work: impl Fn(&mut mysql::PooledConn) -> Result<T>) -> Result<T> {
        let Backend::Pooled(pool) = &*self.backend else { unreachable!("on_shared without a pool") };
        let pool = pool.clone();
        let in_transaction = self.in_transaction;
        waiting(|| {
            let mut tries = 0;
            loop {
                if self.pooled.is_none() {
                    self.pooled = Some(pool.get_conn()?);
                }
                let connection = self.pooled.as_mut().expect("just taken");
                match work(connection) {
                    Err(error) if gone(&error) => {
                        drop(self.pooled.take().map(mysql::PooledConn::unwrap));
                        tries += 1;
                        if in_transaction || tries >= TRIES {
                            return Err(error);
                        }
                    }
                    done => return done,
                }
            }
        })
    }

    fn say(&mut self, sql: &str, params: &[Value]) -> Result<Outcome> {
        let renamed_sql;
        let sql = match self.naming {
            Some(naming) => {
                renamed_sql = renamed(sql, naming);
                renamed_sql.as_str()
            }
            None => sql,
        };
        if matches!(&*self.backend, Backend::Pooled(_)) {
            return self.on_shared(|connection| say_on(connection, sql, params));
        }
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
                    Err(error) if gone(&error) && !in_transaction => {
                        mysql.connection = mysql_connection(&mysql.options)?;
                        mysql.say(sql, params)
                    }
                    answer => answer,
                }
            }),
            Backend::Pooled(_) => unreachable!("said above"),
        }
    }

    /// The store's own tables, by the names its statements give them: for a store
    /// in a database of its own, every table there; for one sharing a database, those
    /// whose names start with its prefix, without it (no store's prefix starts
    /// another's: see [`Location::Shared`]). The services' mark is left out.
    pub fn tables(&mut self) -> Result<Vec<String>> {
        let naming = self.naming.cloned();
        let listed: Vec<String> = match (&*self.backend, &naming) {
            (Backend::Sqlite(_), _) => self.as_written("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name", &[])?,
            (_, Some(Naming { database: Some(database), .. })) => {
                self.as_written("SELECT table_name FROM information_schema.tables WHERE table_schema = ?1 ORDER BY table_name", &[Value::Text(database.clone())])?
            }
            _ => self.as_written("SELECT table_name FROM information_schema.tables WHERE table_schema = DATABASE() ORDER BY table_name", &[])?,
        }
        .rows
        .iter()
        .map(|row| row.get::<String>(0))
        .collect::<Result<_>>()?;
        let prefix = naming.map(|naming| naming.prefix).unwrap_or_default();
        Ok(listed.into_iter().filter_map(|name| name.strip_prefix(prefix.as_str()).map(str::to_string)).filter(|name| name != "wyvrn_service").collect())
    }

    /// Every row of `table`, with its columns' names: text as text and bytes as
    /// bytes, as each column holds them.
    pub fn dump(&mut self, table: &str) -> Result<(Vec<String>, Vec<Vec<Value>>)> {
        let sql = match self.naming {
            Some(naming) => renamed(&format!("SELECT * FROM \"{table}\""), naming),
            None => format!("SELECT * FROM \"{table}\""),
        };
        let read = |connection: &mut dyn MySqlReading| connection.dump(&sql);
        match &mut *self.backend {
            Backend::Sqlite(connection) => {
                let mut statement = connection.prepare(&sql)?;
                let columns: Vec<String> = statement.column_names().into_iter().map(str::to_string).collect();
                let count = columns.len();
                let rows = statement.query_map([], |row| from_sqlite(row, count))?.collect::<rusqlite::Result<Vec<_>>>()?;
                Ok((columns, rows.into_iter().map(|row| row.0).collect()))
            }
            Backend::MySql(mysql) => waiting(|| read(&mut mysql.connection)),
            Backend::Pooled(_) => self.on_shared(|connection| read(connection)),
        }
    }

    /// Says a statement as it is written, its table names untouched.
    fn as_written(&mut self, sql: &str, params: &[Value]) -> Result<Outcome> {
        let naming = self.naming.take();
        let said = self.say(sql, params);
        self.naming = naming;
        said
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
            Backend::MySql(_) | Backend::Pooled(_) => "START TRANSACTION",
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

    /// Says a statement with no parameters and no answer (its tables named as the
    /// store's are).
    fn plainly(&mut self, sql: &str) -> Result<()> {
        let sql = match self.naming {
            Some(naming) => renamed(sql, naming),
            None => sql.to_string(),
        };
        let in_transaction = self.in_transaction;
        match &mut *self.backend {
            Backend::Sqlite(connection) => connection.execute_batch(&sql)?,
            Backend::MySql(mysql) => waiting(|| {
                mysql.ready(in_transaction)?;
                Ok::<_, Error>(mysql.connection.query_drop(&sql)?)
            })?,
            Backend::Pooled(_) => self.on_shared(|connection| Ok(connection.query_drop(&sql)?))?,
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
    fn a_shared_stores_tables_are_named_with_its_prefix_wherever_a_statement_names_one() {
        let alone = Naming { database: None, prefix: "c7_".into() };
        let shared = Naming { database: Some("hosted".into()), prefix: "c7_".into() };
        let cases = [
            ("SELECT a FROM posts WHERE b = 'FROM x' AND c IN (SELECT d FROM members)", r#"SELECT a FROM "c7_posts" WHERE b = 'FROM x' AND c IN (SELECT d FROM "c7_members")"#),
            ("INSERT OR IGNORE INTO \"keys\" (a) VALUES (?1)", r#"INSERT OR IGNORE INTO "c7_keys" (a) VALUES (?1)"#),
            ("UPDATE OR IGNORE roles SET a = 1", r#"UPDATE OR IGNORE "c7_roles" SET a = 1"#),
            ("DELETE FROM t", r#"DELETE FROM "c7_t""#),
            ("SELECT p.a FROM posts p LEFT JOIN reactions r ON r.post = p.id", r#"SELECT p.a FROM "c7_posts" p LEFT JOIN "c7_reactions" r ON r.post = p.id"#),
            ("SELECT COUNT(*) FROM (SELECT a FROM posts)", r#"SELECT COUNT(*) FROM (SELECT a FROM "c7_posts")"#),
            (
                "-- FROM nowhere\nCREATE TABLE IF NOT EXISTS packs (a TEXT);\nCREATE INDEX IF NOT EXISTS packs_owner ON packs (a)",
                "-- FROM nowhere\nCREATE TABLE IF NOT EXISTS \"c7_packs\" (a TEXT);\nCREATE INDEX IF NOT EXISTS \"c7_packs_owner\" ON \"c7_packs\" (a)",
            ),
            ("CREATE TABLE IF NOT EXISTS t (a INT, INDEX (a)) ENGINE=InnoDB", r#"CREATE TABLE IF NOT EXISTS "c7_t" (a INT, INDEX (a)) ENGINE=InnoDB"#),
        ];
        for (sql, expected) in cases {
            assert_eq!(renamed(sql, &alone), expected);
        }
        assert_eq!(renamed("SELECT a FROM posts", &shared), r#"SELECT a FROM "hosted"."c7_posts""#);
        // A table its columns are qualified with keeps its name, unless it has another.
        assert_eq!(
            renamed("SELECT reactions.emoji FROM reactions LEFT JOIN members ON members.did = reactions.did WHERE reactions.seq = ?1", &alone),
            r#"SELECT reactions.emoji FROM "c7_reactions" AS reactions LEFT JOIN "c7_members" AS members ON members.did = reactions.did WHERE reactions.seq = ?1"#
        );
        assert_eq!(renamed("SELECT r.a FROM reactions r WHERE reactions.b", &alone), r#"SELECT r.a FROM "c7_reactions" r WHERE reactions.b"#);
        assert_eq!(renamed("UPDATE posts SET a = 1 WHERE posts.b = 2", &alone), r#"UPDATE "c7_posts" AS posts SET a = 1 WHERE posts.b = 2"#);
        // Nothing else is touched: from, an ON of a join, a word that only looks like a table.
        assert_eq!(renamed("SELECT 'it''s' FROM t WHERE x = ?1", &alone), r#"SELECT 'it''s' FROM "c7_t" WHERE x = ?1"#);
    }

    #[test]
    fn two_stores_with_prefixes_share_a_sqlite_database_and_see_only_their_own() {
        // One file for both, as in a test: two connections to a shared in-memory database.
        let name = format!("file:shared_{}?mode=memory&cache=shared", scratch_name());
        let open = |prefix: &str| {
            let connection = rusqlite::Connection::open_with_flags(&name, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI).unwrap();
            Db::sqlite(connection, &SCHEMA, Some(Naming { database: None, prefix: prefix.into() })).unwrap()
        };
        let (one, two) = (open("a_"), open("b_"));
        one.lock().execute("INSERT INTO things (name, size) VALUES (?1, 1)", params!["one's"]).unwrap();
        two.lock().execute("INSERT INTO things (name, size) VALUES (?1, 2)", params!["one's"]).unwrap();
        assert_eq!(one.lock().query_value::<i64>("SELECT size FROM things WHERE name = ?1", params!["one's"]).unwrap(), 1);
        assert_eq!(two.lock().query_value::<i64>("SELECT COUNT(*) FROM things", params![]).unwrap(), 1);
    }

    #[test]
    fn shared_mysql_stores_keep_apart_share_connections_and_are_each_one_services() {
        let Some(url) = server() else { return };
        let Location::MySql(url) = Location::parse(&url).unwrap() else { panic!("WYVRN_TEST_DATABASE_URL is for a MySQL server") };
        let server: mysql::Opts = mysql::OptsBuilder::from_opts(mysql_options(&url).unwrap()).db_name(None::<String>).into();
        let database = format!("wyvrn_test_{}", scratch_name());
        let one = Db::shared_with(server.clone(), &database, "c1_", &SCHEMA, "community").unwrap();
        let two = Db::shared_with(server.clone(), &database, "c2_", &SCHEMA, "community").unwrap();
        one.lock().execute("INSERT INTO things (name, size) VALUES (?1, 1)", params!["same"]).unwrap();
        two.lock().execute("INSERT INTO things (name, size) VALUES (?1, 2)", params!["same"]).unwrap();
        assert_eq!(one.lock().query_value::<i64>("SELECT size FROM things WHERE name = ?1", params!["same"]).unwrap(), 1);
        assert_eq!(two.lock().query_value::<i64>("SELECT size FROM things WHERE name = ?1", params!["same"]).unwrap(), 2);
        // Another service under the same prefix is refused, as on a database of its own.
        assert!(Db::shared_with(server.clone(), &database, "c1_", &SCHEMA, "stickers").is_err());
        assert!(Db::shared_with(server.clone(), &database, "c1-x", &SCHEMA, "community").is_err());

        // A transaction holds its connection; one that fails leaves nothing.
        let failed: Result<()> = one.lock().transaction(|session| {
            session.execute("UPDATE things SET size = 100", params![])?;
            Err(Error::Other("no".into()))
        });
        assert!(failed.is_err());
        assert_eq!(one.lock().query_value::<i64>("SELECT size FROM things", params![]).unwrap(), 1);

        // A connection killed under it: the statement is said again on another.
        let mut root = mysql::Conn::new(mysql_options(&url).unwrap()).unwrap();
        let connection: i64 = one.lock().query_value("SELECT CONNECTION_ID()", params![]).unwrap();
        root.query_drop(format!("KILL {connection}")).unwrap();
        assert_eq!(one.lock().query_value::<i64>("SELECT size FROM things", params![]).unwrap(), 1);
    }

    #[test]
    fn a_store_is_copied_whole_between_kinds_of_database_text_staying_text() {
        let dir = std::env::temp_dir().join(format!("wyvrn-copy-test-{}", scratch_name()));
        let (from, to) = (Location::Sqlite(dir.join("from.sqlite")), Location::Sqlite(dir.join("to.sqlite")));
        let source = Db::open(&from, &SCHEMA, "community").unwrap();
        source.lock().execute("INSERT INTO things (name, size, bytes) VALUES (?1, 1, ?2)", params!["ünï", [0u8, 255].as_slice()]).unwrap();
        source.lock().execute("INSERT INTO things (name, size, note) VALUES (?1, 2, ?2)", params!["two", "a note"]).unwrap();
        drop(source);
        let check = |location: &Location| {
            let db = Db::open(location, &SCHEMA, "community").unwrap();
            let mut session = db.lock();
            assert_eq!(session.tables().unwrap(), vec!["things".to_string()]);
            // Found by its text, so stored as text; the bytes as bytes.
            assert_eq!(session.query_value::<i64>("SELECT size FROM things WHERE name = ?1", params!["ünï"]).unwrap(), 1);
            assert_eq!(session.query_value::<Vec<u8>>("SELECT bytes FROM things WHERE size = 1", params![]).unwrap(), vec![0, 255]);
            // Numbering goes on after what was copied.
            assert_eq!(session.insert("INSERT INTO things (name, size) VALUES ('three', 3)", params![]).unwrap(), 3);
        };
        Db::open(&to, &SCHEMA, "community").unwrap();
        assert_eq!(copy_store(&from, &to, "community", &[]).unwrap(), vec![("things".to_string(), 2)]);
        check(&to);
        // Never over what is there.
        assert!(copy_store(&from, &to, "community", &[]).is_err());

        // Into MySQL tables of its own, and back out, where there is a server.
        if let Some(url) = server() {
            let server = url.trim_end_matches('/').to_string();
            let shared = Location::Shared { server, database: format!("wyvrn_test_{}", scratch_name()), prefix: "c9_m_".into() };
            Db::open(&shared, &SCHEMA, "community").unwrap();
            copy_store(&from, &shared, "community", &[]).unwrap();
            let back = Location::Sqlite(dir.join("back.sqlite"));
            Db::open(&back, &SCHEMA, "community").unwrap();
            copy_store(&shared, &back, "community", &[]).unwrap();
            check(&back);
            check(&shared);
        }
        std::fs::remove_dir_all(&dir).unwrap();
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
