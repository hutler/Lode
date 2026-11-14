//! The one door to the database (T152): every statement of the server and its modules runs
//! through [`Sql`], which SQLite and Postgres both implement.
//!
//! Statements are written once, in the subset both databases understand:
//!
//! - parameters are numbered `?1`, `?2`, … (rewritten to `$1`, `$2`, … for Postgres); a bare
//!   `?` is numbered in order of appearance;
//! - `RETURNING` instead of `last_insert_rowid()`, `ON CONFLICT … DO NOTHING / DO UPDATE`
//!   instead of `INSERT OR IGNORE / REPLACE`;
//! - integers are 64-bit, booleans are stored as 0 and 1, byte strings are `BLOB` (`BYTEA`);
//! - an aggregate such as `SUM` is cast back to `BIGINT` where its type matters;
//! - no SQLite-only functions. Where a statement truly differs, [`Sql::dialect`] picks the form.
//!
//! Results come back as owned [`Row`]s.

use std::borrow::Cow;

use crate::{MetaError, Result};

/// Which database a connection speaks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// SQLite (the default, `<data>/meta.db`).
    Sqlite,
    /// Postgres (T153).
    Postgres,
}

/// A statement parameter.
#[derive(Debug, Clone, PartialEq)]
pub enum Val<'a> {
    /// `NULL`.
    Null,
    /// A 64-bit integer (also booleans, as 0 and 1).
    Int(i64),
    /// Text.
    Text(Cow<'a, str>),
    /// Bytes.
    Blob(Cow<'a, [u8]>),
}

impl From<i64> for Val<'_> {
    fn from(v: i64) -> Self {
        Val::Int(v)
    }
}
impl From<i32> for Val<'_> {
    fn from(v: i32) -> Self {
        Val::Int(i64::from(v))
    }
}
impl From<u32> for Val<'_> {
    fn from(v: u32) -> Self {
        Val::Int(i64::from(v))
    }
}
impl From<u16> for Val<'_> {
    fn from(v: u16) -> Self {
        Val::Int(v.into())
    }
}

impl From<u8> for Val<'_> {
    fn from(v: u8) -> Self {
        Val::Int(i64::from(v))
    }
}
impl From<bool> for Val<'_> {
    fn from(v: bool) -> Self {
        Val::Int(i64::from(v))
    }
}
impl<'a> From<&'a str> for Val<'a> {
    fn from(v: &'a str) -> Self {
        Val::Text(Cow::Borrowed(v))
    }
}
impl<'a> From<&'a String> for Val<'a> {
    fn from(v: &'a String) -> Self {
        Val::Text(Cow::Borrowed(v.as_str()))
    }
}
impl From<String> for Val<'_> {
    fn from(v: String) -> Self {
        Val::Text(Cow::Owned(v))
    }
}
impl<'a> From<&'a [u8]> for Val<'a> {
    fn from(v: &'a [u8]) -> Self {
        Val::Blob(Cow::Borrowed(v))
    }
}
impl<'a> From<&'a Vec<u8>> for Val<'a> {
    fn from(v: &'a Vec<u8>) -> Self {
        Val::Blob(Cow::Borrowed(v.as_slice()))
    }
}
impl From<Vec<u8>> for Val<'_> {
    fn from(v: Vec<u8>) -> Self {
        Val::Blob(Cow::Owned(v))
    }
}
impl<'a> From<&'a [u8; 32]> for Val<'a> {
    fn from(v: &'a [u8; 32]) -> Self {
        Val::Blob(Cow::Borrowed(v.as_slice()))
    }
}
impl<'a, T: Into<Val<'a>>> From<Option<T>> for Val<'a> {
    fn from(v: Option<T>) -> Self {
        v.map_or(Val::Null, Into::into)
    }
}
impl<'a, T: Clone + Into<Val<'a>>> From<&Option<T>> for Val<'a> {
    fn from(v: &Option<T>) -> Self {
        v.clone().map_or(Val::Null, Into::into)
    }
}

/// Builds a parameter list: `vals![depot.0, name, Some(3)]`.
#[macro_export]
macro_rules! vals {
    () => { &[] as &[$crate::Val<'_>] };
    ($($v:expr),+ $(,)?) => { &[$($crate::Val::from($v)),+][..] };
}

/// One value of a result row.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    /// `NULL`.
    Null,
    /// An integer.
    Int(i64),
    /// A floating-point number.
    Real(f64),
    /// Text.
    Text(String),
    /// Bytes.
    Blob(Vec<u8>),
}

/// A result row.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Row {
    cells: Vec<Cell>,
}

impl Row {
    /// A row of these cells.
    pub fn new(cells: Vec<Cell>) -> Self {
        Self { cells }
    }

    /// Column `i` as `T`.
    pub fn get<T: FromCell>(&self, i: usize) -> Result<T> {
        let cell = self
            .cells
            .get(i)
            .ok_or_else(|| MetaError::BadValue(format!("no column {i}")))?;
        T::from_cell(cell).map_err(|why| MetaError::BadValue(format!("column {i}: {why}")))
    }

    /// Number of columns.
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// Whether the row has no columns.
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }
}

/// Conversion from a result cell.
pub trait FromCell: Sized {
    /// Converts, or says why not.
    fn from_cell(c: &Cell) -> std::result::Result<Self, String>;
}

impl FromCell for i64 {
    fn from_cell(c: &Cell) -> std::result::Result<Self, String> {
        match c {
            Cell::Int(v) => Ok(*v),
            other => Err(format!("expected an integer, found {other:?}")),
        }
    }
}
impl FromCell for u64 {
    fn from_cell(c: &Cell) -> std::result::Result<Self, String> {
        let v = i64::from_cell(c)?;
        u64::try_from(v).map_err(|_| format!("{v} is negative"))
    }
}
impl FromCell for u32 {
    fn from_cell(c: &Cell) -> std::result::Result<Self, String> {
        let v = i64::from_cell(c)?;
        u32::try_from(v).map_err(|_| format!("{v} does not fit in u32"))
    }
}
impl FromCell for u16 {
    fn from_cell(c: &Cell) -> std::result::Result<Self, String> {
        let v = i64::from_cell(c)?;
        u16::try_from(v).map_err(|_| format!("{v} does not fit in u16"))
    }
}
impl FromCell for u8 {
    fn from_cell(c: &Cell) -> std::result::Result<Self, String> {
        let v = i64::from_cell(c)?;
        u8::try_from(v).map_err(|_| format!("{v} does not fit in u8"))
    }
}
impl FromCell for bool {
    fn from_cell(c: &Cell) -> std::result::Result<Self, String> {
        Ok(i64::from_cell(c)? != 0)
    }
}
impl FromCell for f64 {
    fn from_cell(c: &Cell) -> std::result::Result<Self, String> {
        match c {
            Cell::Real(v) => Ok(*v),
            Cell::Int(v) => Ok(*v as f64),
            other => Err(format!("expected a number, found {other:?}")),
        }
    }
}
impl FromCell for String {
    fn from_cell(c: &Cell) -> std::result::Result<Self, String> {
        match c {
            Cell::Text(v) => Ok(v.clone()),
            other => Err(format!("expected text, found {other:?}")),
        }
    }
}
impl FromCell for Vec<u8> {
    fn from_cell(c: &Cell) -> std::result::Result<Self, String> {
        match c {
            Cell::Blob(v) => Ok(v.clone()),
            other => Err(format!("expected bytes, found {other:?}")),
        }
    }
}
impl FromCell for lode_core::ObjectId {
    fn from_cell(c: &Cell) -> std::result::Result<Self, String> {
        let b = Vec::<u8>::from_cell(c)?;
        let arr: [u8; 32] = b.try_into().map_err(|_| "object id is not 32 bytes")?;
        Ok(lode_core::ObjectId::from_bytes(arr))
    }
}
impl<T: FromCell> FromCell for Option<T> {
    fn from_cell(c: &Cell) -> std::result::Result<Self, String> {
        match c {
            Cell::Null => Ok(None),
            other => T::from_cell(other).map(Some),
        }
    }
}

/// A connection or transaction that runs statements. Object safe; the convenience methods
/// live on `dyn Sql`.
pub trait Sql {
    /// Which database this is.
    fn dialect(&self) -> Dialect;
    /// Runs a statement and returns the number of rows it changed.
    fn exec(&self, sql: &str, params: &[Val<'_>]) -> Result<usize>;
    /// Runs a query and returns every row.
    fn rows(&self, sql: &str, params: &[Val<'_>]) -> Result<Vec<Row>>;
    /// Runs several statements without parameters, such as a migration.
    fn batch(&self, sql: &str) -> Result<()>;
}

impl dyn Sql + '_ {
    /// Runs a statement and returns the number of rows it changed.
    pub fn execute(&self, sql: &str, params: &[Val<'_>]) -> Result<usize> {
        self.exec(sql, params)
    }

    /// Every row, converted by `f`.
    pub fn query<T>(
        &self,
        sql: &str,
        params: &[Val<'_>],
        mut f: impl FnMut(&Row) -> Result<T>,
    ) -> Result<Vec<T>> {
        self.rows(sql, params)?.iter().map(&mut f).collect()
    }

    /// The first row converted by `f`, or `None` when there is none.
    pub fn query_opt<T>(
        &self,
        sql: &str,
        params: &[Val<'_>],
        f: impl FnOnce(&Row) -> Result<T>,
    ) -> Result<Option<T>> {
        match self.rows(sql, params)?.first() {
            Some(r) => f(r).map(Some),
            None => Ok(None),
        }
    }

    /// The first row converted by `f`; [`MetaError::NotFound`] when there is none.
    pub fn query_one<T>(
        &self,
        sql: &str,
        params: &[Val<'_>],
        f: impl FnOnce(&Row) -> Result<T>,
    ) -> Result<T> {
        self.query_opt(sql, params, f)?
            .ok_or_else(|| MetaError::NotFound("no row".into()))
    }

    /// The first column of the first row.
    pub fn value<T: FromCell>(&self, sql: &str, params: &[Val<'_>]) -> Result<T> {
        self.query_one(sql, params, |r| r.get(0))
    }
}

// ---- SQLite ----

impl Sql for rusqlite::Connection {
    fn dialect(&self) -> Dialect {
        Dialect::Sqlite
    }

    fn exec(&self, sql: &str, params: &[Val<'_>]) -> Result<usize> {
        let mut st = self.prepare_cached(sql)?;
        Ok(st.execute(rusqlite::params_from_iter(params.iter().map(SqliteVal)))?)
    }

    fn rows(&self, sql: &str, params: &[Val<'_>]) -> Result<Vec<Row>> {
        let mut st = self.prepare_cached(sql)?;
        let n = st.column_count();
        let mut rows = st.query(rusqlite::params_from_iter(params.iter().map(SqliteVal)))?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let mut cells = Vec::with_capacity(n);
            for i in 0..n {
                use rusqlite::types::ValueRef;
                cells.push(match r.get_ref(i)? {
                    ValueRef::Null => Cell::Null,
                    ValueRef::Integer(v) => Cell::Int(v),
                    ValueRef::Real(v) => Cell::Real(v),
                    ValueRef::Text(t) => Cell::Text(
                        std::str::from_utf8(t)
                            .map_err(|_| MetaError::BadValue("text is not UTF-8".into()))?
                            .to_owned(),
                    ),
                    ValueRef::Blob(b) => Cell::Blob(b.to_vec()),
                });
            }
            out.push(Row { cells });
        }
        Ok(out)
    }

    fn batch(&self, sql: &str) -> Result<()> {
        Ok(self.execute_batch(sql)?)
    }
}

struct SqliteVal<'v, 'a>(&'v Val<'a>);

impl rusqlite::ToSql for SqliteVal<'_, '_> {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        use rusqlite::types::{ToSqlOutput, ValueRef};
        Ok(match self.0 {
            Val::Null => ToSqlOutput::Borrowed(ValueRef::Null),
            Val::Int(v) => ToSqlOutput::Borrowed(ValueRef::Integer(*v)),
            Val::Text(t) => ToSqlOutput::Borrowed(ValueRef::Text(t.as_bytes())),
            Val::Blob(b) => ToSqlOutput::Borrowed(ValueRef::Blob(b)),
        })
    }
}

/// Rewrites `?N` and bare `?` parameters to Postgres's `$N`, leaving quoted text alone.
pub fn to_postgres_params(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len() + 8);
    let mut next = 1usize;
    let mut chars = sql.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            out.push(c);
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                out.push(c);
            }
            '?' => {
                let mut digits = String::new();
                while let Some(d) = chars.peek().copied().filter(char::is_ascii_digit) {
                    digits.push(d);
                    chars.next();
                }
                out.push('$');
                if digits.is_empty() {
                    out.push_str(&next.to_string());
                    next += 1;
                } else {
                    out.push_str(&digits);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_parameters_outside_quotes() {
        assert_eq!(
            to_postgres_params("SELECT a FROM t WHERE x = ?1 AND y = '?2' AND z IN (?, ?)"),
            "SELECT a FROM t WHERE x = $1 AND y = '?2' AND z IN ($1, $2)"
        );
        assert_eq!(to_postgres_params("v = ?12"), "v = $12");
    }

    #[test]
    fn sqlite_round_trip() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        let db: &dyn Sql = &c;
        db.batch("CREATE TABLE t (i INTEGER, s TEXT, b BLOB)")
            .unwrap();
        let n = db
            .execute(
                "INSERT INTO t VALUES (?1, ?2, ?3)",
                vals![7i64, "x", vec![1u8, 2]],
            )
            .unwrap();
        assert_eq!(n, 1);
        db.execute(
            "INSERT INTO t VALUES (?1, ?2, ?3)",
            vals![None::<i64>, None::<&str>, None::<Vec<u8>>],
        )
        .unwrap();
        let rows = db
            .query("SELECT i, s, b FROM t ORDER BY i", vals![], |r| {
                Ok((
                    r.get::<Option<i64>>(0)?,
                    r.get::<Option<String>>(1)?,
                    r.get::<Option<Vec<u8>>>(2)?,
                ))
            })
            .unwrap();
        assert_eq!(rows[0], (None, None, None));
        assert_eq!(rows[1], (Some(7), Some("x".into()), Some(vec![1, 2])));
        assert_eq!(
            db.value::<i64>("SELECT count(*) FROM t", vals![]).unwrap(),
            2
        );
        assert!(db
            .query_opt("SELECT i FROM t WHERE i = ?1", vals![99i64], |r| r
                .get::<i64>(0))
            .unwrap()
            .is_none());
    }
}
