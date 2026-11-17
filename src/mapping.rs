//! Audited, dataset-scoped imports of human-reviewed Wikidata decisions.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use csv::ReaderBuilder;
use diesel::Connection as _;
use diesel::SqliteConnection;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::ensure_observation_subject_column;

const HEADER: &[&str] = &["source_value", "wikidata_qid", "reviewer", "rationale"];
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS kg_mapping_imports (
    id INTEGER PRIMARY KEY,
    dataset_id INTEGER NOT NULL REFERENCES datasets(id),
    source_field TEXT NOT NULL,
    file_sha256 TEXT NOT NULL,
    source_path TEXT NOT NULL,
    imported_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    active INTEGER NOT NULL CHECK (active IN (0, 1))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_kg_mapping_active
    ON kg_mapping_imports(dataset_id, source_field) WHERE active = 1;
CREATE TABLE IF NOT EXISTS kg_mapping_decisions (
    import_id INTEGER NOT NULL REFERENCES kg_mapping_imports(id),
    source_value TEXT NOT NULL,
    wikidata_qid TEXT NOT NULL,
    reviewer TEXT NOT NULL,
    rationale TEXT NOT NULL,
    PRIMARY KEY (import_id, source_value)
);
";

struct Decision {
    wikidata_qid: String,
    reviewer: String,
    rationale: String,
}

/// The active import's identity and the number of reviewed decisions.
pub struct ImportReport {
    pub version: i64,
    pub sha256: String,
    pub rows: usize,
    pub unchanged: bool,
}

fn parse_review(file: &Path) -> Result<(HashMap<String, Decision>, String)> {
    let bytes = fs::read(file).with_context(|| format!("read mapping file {}", file.display()))?;
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let mut reader = ReaderBuilder::new()
        .delimiter(b'\t')
        .has_headers(false)
        .quoting(false)
        .from_reader(bytes.as_slice());
    let mut records = reader.records();
    let header = records.next().context("mapping file is empty")??;
    ensure!(header.len() == HEADER.len() && header.iter().zip(HEADER).all(|(actual, expected)| actual == *expected), "mapping file header must be source_value<TAB>wikidata_qid<TAB>reviewer<TAB>rationale");
    let mut decisions = HashMap::new();
    for (index, record) in records.enumerate() {
        let record = record.with_context(|| format!("invalid mapping row {}", index + 2))?;
        ensure!(record.len() == 4, "mapping row {} must have four tab-separated fields", index + 2);
        let source_value = &record[0];
        ensure!(!source_value.trim().is_empty(), "mapping row {} has an empty source value", index + 2);
        ensure!(!decisions.contains_key(source_value), "duplicate source value {source_value:?}");
        let wikidata_qid = &record[1];
        let valid_qid = wikidata_qid.strip_prefix('Q').is_some_and(|digits| {
            digits.as_bytes().first().is_some_and(|digit| (b'1'..=b'9').contains(digit))
                && digits.as_bytes()[1..].iter().all(u8::is_ascii_digit)
        });
        ensure!(valid_qid, "mapping row {} has invalid Wikidata QID {wikidata_qid:?}", index + 2);
        ensure!(!record[2].trim().is_empty(), "mapping row {} has an empty reviewer", index + 2);
        ensure!(!record[3].trim().is_empty(), "mapping row {} has an empty rationale", index + 2);
        decisions.insert(source_value.to_owned(), Decision {
            wikidata_qid: wikidata_qid.to_owned(),
            reviewer: record[2].to_owned(),
            rationale: record[3].to_owned(),
        });
    }
    Ok((decisions, sha256))
}

fn open_existing(db: &Path) -> Result<Connection> {
    ensure!(db.is_file(), "database does not exist: {}", db.display());
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .with_context(|| format!("open existing database {}", db.display()))?;
    conn.busy_timeout(Duration::from_secs(30))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

/// Import a reviewed TSV, retaining every previous version for audit.
///
/// Only an identical *active* file is a no-op. Re-importing a historical file
/// creates a new version so that the sequence of approvals remains observable.
pub fn import(db: &Path, dataset_id: i64, field: &str, file: &Path) -> Result<ImportReport> {
    ensure!(matches!(field, "observation_subject" | "collector_full_name"), "unsupported mapping field {field:?}");
    let (decisions, sha256) = parse_review(file)?;
    let source_path = file.to_str().context("mapping path must be UTF-8")?;

    // Fail before any migration for a missing database or dataset. The Diesel
    // helper owns the legacy-column migration and leaves the original JSON intact.
    let conn = open_existing(db)?;
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM datasets WHERE id = ?1)",
        [dataset_id],
        |row| row.get(0),
    )?;
    ensure!(exists, "dataset {dataset_id} does not exist");
    drop(conn);
    if field == "observation_subject" {
        let mut diesel_conn = SqliteConnection::establish(db.to_str().context("database path must be UTF-8")?)
            .context("open SQLite database for observation subject migration")?;
        ensure_observation_subject_column(&mut diesel_conn)?;
    }

    let mut conn = open_existing(db)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Hold the writer reservation while checking the source values and active
    // version, so concurrent importers cannot both activate competing versions.
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM datasets WHERE id = ?1)",
        [dataset_id],
        |row| row.get(0),
    )?;
    ensure!(exists, "dataset {dataset_id} does not exist");
    {
        let source_sql = match field {
            "observation_subject" =>
                "SELECT EXISTS(SELECT 1 FROM sample_rows
                 WHERE dataset_id = ?1 AND observation_subject = ?2
                   AND json_extract(attributes_json, '$.observation_subject') = ?2)",
            "collector_full_name" =>
                "SELECT EXISTS(SELECT 1 FROM sample_rows
                 WHERE dataset_id = ?1 AND json_extract(attributes_json, '$.collector_full_name') = ?2)",
            _ => unreachable!("field checked above"),
        };
        let mut check = tx.prepare(source_sql)?;
        for source_value in decisions.keys() {
            let matches: bool = check.query_row(params![dataset_id, source_value], |row| row.get(0))?;
            ensure!(matches, "source value {source_value:?} is absent from dataset {dataset_id} {field}");
        }
    }
    tx.execute_batch(SCHEMA)?;
    let active = tx.query_row(
        "SELECT id, file_sha256 FROM kg_mapping_imports
         WHERE dataset_id = ?1 AND source_field = ?2 AND active = 1",
        params![dataset_id, field],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
    ).optional()?;
    if let Some((version, active_sha)) = &active {
        if active_sha == &sha256 {
            tx.commit()?;
            return Ok(ImportReport { version: *version, sha256, rows: decisions.len(), unchanged: true });
        }
    }
    if let Some((version, _)) = active {
        tx.execute("UPDATE kg_mapping_imports SET active = 0 WHERE id = ?1", [version])?;
    }
    tx.execute(
        "INSERT INTO kg_mapping_imports (dataset_id, source_field, file_sha256, source_path, active)
         VALUES (?1, ?2, ?3, ?4, 1)",
        params![dataset_id, field, sha256, source_path],
    )?;
    let version = tx.last_insert_rowid();
    {
        let mut insert = tx.prepare(
            "INSERT INTO kg_mapping_decisions (import_id, source_value, wikidata_qid, reviewer, rationale)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for (source_value, decision) in &decisions {
            insert.execute(params![version, source_value, decision.wikidata_qid, decision.reviewer, decision.rationale])?;
        }
    }
    tx.commit()?;
    Ok(ImportReport { version, sha256, rows: decisions.len(), unchanged: false })
}
