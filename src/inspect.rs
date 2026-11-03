//! Stream SQLite tables to TSV without creating or modifying the database.
use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, ToSql};

pub(crate) fn open(database: &Path) -> Result<Connection> {
    Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open SQLite database {} read-only", database.display()))
}

/// Write user table and view names, one per line, without opening the database for writes.
pub fn list_tables(database: &Path, output: impl Write) -> Result<()> {
    let db = open(database)?;
    let mut writer = csv::WriterBuilder::new()
        .delimiter(b'\t')
        .from_writer(output);
    writer.write_record(["relation"])?;
    let mut statement = db.prepare(
        "SELECT name FROM sqlite_master WHERE type IN ('table', 'view') AND name NOT GLOB 'sqlite_*' ORDER BY name",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        writer.write_record([row.get::<_, String>(0)?])?;
    }
    writer.flush()?;
    Ok(())
}

/// Stream an existing user table or view's headers and rows as TSV; `None` exports every row.
/// SQLite NULL is an empty field and BLOB values are `0x`-prefixed hexadecimal.
pub fn write_table(
    database: &Path,
    table: &str,
    limit: Option<usize>,
    output: impl Write,
) -> Result<()> {
    let db = open(database)?;
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type IN ('table', 'view') AND name = ?1 AND name NOT GLOB 'sqlite_*')",
        [table],
        |row| row.get(0),
    )?;
    if !exists {
        bail!("table {table:?} not found");
    }
    let escaped = table.replace('"', "\"\"");
    let sql = format!("SELECT * FROM \"{escaped}\" LIMIT ?1");
    let limit = limit.map(i64::try_from).transpose()?.unwrap_or(-1);
    write_query(&db, &sql, &[&limit], output)
}

/// Stream a read-only report with the same TSV encoding as `show` and `export`.
pub(crate) fn write_query(
    db: &Connection,
    sql: &str,
    values: &[&dyn ToSql],
    output: impl Write,
) -> Result<()> {
    let mut statement = db.prepare(sql)?;
    let mut writer = csv::WriterBuilder::new()
        .delimiter(b'\t')
        .from_writer(output);
    writer.write_record(statement.column_names())?;
    let column_count = statement.column_count();
    let mut rows = statement.query(values)?;
    while let Some(row) = rows.next()? {
        for index in 0..column_count {
            match row.get_ref(index)? {
                ValueRef::Null => writer.write_field(b"")?,
                ValueRef::Integer(number) => writer.write_field(number.to_string().as_bytes())?,
                ValueRef::Real(number) => writer.write_field(number.to_string().as_bytes())?,
                ValueRef::Text(bytes) => writer.write_field(bytes)?,
                ValueRef::Blob(bytes) => {
                    let mut hex = String::with_capacity(2 + bytes.len() * 2);
                    hex.push_str("0x");
                    for byte in bytes {
                        write!(hex, "{byte:02x}")?;
                    }
                    writer.write_field(hex.as_bytes())?;
                }
            }
        }
        writer.write_record(None::<&[u8]>)?;
    }
    writer.flush()?;
    Ok(())
}
