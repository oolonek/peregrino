//! Import metadata only after machaca validates the selected spectrum references.

mod artifact;
pub mod annotations;
pub mod inspect;
pub mod kg;
pub mod kg_validation;
pub mod register;
use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use diesel::connection::SimpleConnection;
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Text};
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::{Map, Value};

/// One dataset ingestion; paths to metadata and spectra are relative to `root`.
pub struct IngestOptions {
    pub root: PathBuf,
    pub doi: String,
    pub metadata: PathBuf,
    pub taxon_column: String,
    pub ms_dir: PathBuf,
    pub database: PathBuf,
    pub verbose: bool,
    /// Validate and import only the first N metadata records; `None` imports all rows.
    pub max_rows: Option<usize>,
}

/// Committed row and spectrum counts.
#[derive(Debug, PartialEq, Eq)]
pub struct Counts {
    pub rows: u64,
    pub spectra: u64,
}

#[derive(QueryableByName)]
struct DatasetId {
    #[diesel(sql_type = BigInt)]
    id: i64,
}

#[derive(QueryableByName)]
struct ColumnCount {
    #[diesel(sql_type = BigInt)]
    count: i64,
}

#[derive(QueryableByName)]
struct PreservedSpectrum {
    #[diesel(sql_type = Text)]
    filename: String,
    #[diesel(sql_type = Text)]
    relative_path: String,
    #[diesel(sql_type = Text)]
    ms2_path: String,
    #[diesel(sql_type = Text)]
    ms2_sha256: String,
    #[diesel(sql_type = Text)]
    features_path: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
    paired_path: Option<String>,
    #[diesel(sql_type = Text)]
    raw_identity: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
    registered_at: Option<String>,
}

pub(crate) const PROCESSED_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS processed_spectra (
    dataset_id INTEGER NOT NULL,
    filename TEXT NOT NULL,
    ms2_path TEXT NOT NULL,
    ms2_sha256 TEXT NOT NULL,
    features_path TEXT NOT NULL,
    paired_path TEXT,
    registered_at TEXT,
    raw_identity TEXT NOT NULL,
    PRIMARY KEY (dataset_id, filename),
    FOREIGN KEY (dataset_id, filename) REFERENCES spectrum_files(dataset_id, filename) ON DELETE CASCADE
);
"#;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS datasets (
    id INTEGER PRIMARY KEY,
    doi TEXT NOT NULL,
    root TEXT NOT NULL,
    metadata_path TEXT NOT NULL,
    ms_dir TEXT NOT NULL,
    UNIQUE (doi, root)
);
CREATE TABLE IF NOT EXISTS taxa (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE CHECK (length(trim(name)) > 0)
);
CREATE TABLE IF NOT EXISTS sample_rows (
    dataset_id INTEGER NOT NULL REFERENCES datasets(id) ON DELETE CASCADE,
    row_number INTEGER NOT NULL,
    sample_id TEXT,
    taxon_id INTEGER REFERENCES taxa(id),
    attributes_json TEXT NOT NULL CHECK (json_valid(attributes_json)),
    PRIMARY KEY (dataset_id, row_number)
);
CREATE TABLE IF NOT EXISTS spectrum_files (
    dataset_id INTEGER NOT NULL,
    filename TEXT NOT NULL,
    row_number INTEGER NOT NULL,
    column_name TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    PRIMARY KEY (dataset_id, filename),
    FOREIGN KEY (dataset_id, row_number) REFERENCES sample_rows(dataset_id, row_number) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_sample_id ON sample_rows(sample_id);
-- Foreign-key checks for replacing sample rows must find child spectra by row number.
CREATE INDEX IF NOT EXISTS idx_spectrum_row ON spectrum_files(dataset_id, row_number);
"#;

fn inside(root: &Path, path: &Path, directory: bool) -> Result<PathBuf> {
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let full = full
        .canonicalize()
        .with_context(|| format!("{}", full.display()))?;
    if !full.starts_with(root) || (directory && !full.is_dir()) || (!directory && !full.is_file()) {
        bail!(
            "{} must be a {} inside the dataset",
            full.display(),
            if directory { "directory" } else { "file" }
        );
    }
    Ok(full)
}

fn text(path: &Path) -> Result<&str> {
    path.to_str().context("non-UTF-8 dataset path")
}

fn reader(path: &Path) -> Result<csv::Reader<File>> {
    let delimiter = if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("csv"))
    {
        b','
    } else {
        b'\t'
    };
    Ok(csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .from_path(path)?)
}

/// Read metadata headers using the same dataset path and delimiter rules as ingestion.
pub fn metadata_headers(root: &Path, metadata: &Path) -> Result<csv::StringRecord> {
    let root = root.canonicalize().context("dataset root")?;
    if !root.is_dir() {
        bail!("dataset root must be a directory");
    }
    let metadata = inside(&root, metadata, false)?;
    let mut csv = reader(&metadata)?;
    Ok(csv.headers()?.clone())
}

/// Validate through machaca, then replace this dataset's selected rows in one transaction.
/// A failed validation or import leaves any previously committed dataset intact.
pub fn ingest(options: &IngestOptions) -> Result<Counts> {
    if options.doi.trim().is_empty() {
        bail!("DOI cannot be empty");
    }
    if options.taxon_column.is_empty() {
        bail!("taxon_column must not be empty");
    }
    let root = options.root.canonicalize().context("dataset root")?;
    if !root.is_dir() {
        bail!("dataset root must be a directory");
    }
    let metadata = inside(&root, &options.metadata, false)?;
    let ms_dir = inside(&root, &options.ms_dir, true)?;
    let checked = match options.max_rows {
        Some(max_rows) => machaca::check_first(&metadata, &ms_dir, max_rows),
        None => machaca::check(&metadata, &ms_dir),
    }
    .map_err(anyhow::Error::msg)?;
    let mut csv = reader(&metadata)?;
    let headers = csv.headers()?.clone();
    if headers.iter().any(str::is_empty)
        || headers
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != headers.len()
    {
        bail!("metadata headers must be nonempty and unique");
    }
    let sample_id_index = headers.iter().position(|h| h == "sample_id");
    let taxon_index = headers
        .iter()
        .position(|header| header == options.taxon_column.as_str())
        .with_context(|| format!("taxon column {:?} is absent from metadata headers", options.taxon_column))?;
    if let Some(parent) = options
        .database
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create database directory {}", parent.display()))?;
    }
    let mut db =
        SqliteConnection::establish(text(&options.database)?).context("open SQLite database")?;
    db.batch_execute("PRAGMA foreign_keys = ON;")?;
    db.batch_execute(SCHEMA)?;
    let taxon_column = diesel::sql_query(
        "SELECT COUNT(*) AS count FROM pragma_table_info('sample_rows') WHERE name = 'taxon_id'",
    )
    .get_result::<ColumnCount>(&mut db)?;
    if taxon_column.count == 0 {
        db.batch_execute(
            "ALTER TABLE sample_rows ADD COLUMN taxon_id INTEGER REFERENCES taxa(id)",
        )?;
    }
    db.batch_execute("CREATE INDEX IF NOT EXISTS idx_sample_taxon ON sample_rows(taxon_id)")?;
    let progress = ProgressBar::new_spinner();
    progress.set_style(ProgressStyle::with_template(
        "{spinner:.cyan} [{elapsed_precise}] {pos} sample rows imported",
    )?);
    progress.enable_steady_tick(std::time::Duration::from_millis(120));
    let result = db.transaction::<Counts, anyhow::Error, _>(|conn| {
        conn.batch_execute(PROCESSED_SCHEMA)?;
        diesel::sql_query("INSERT INTO datasets (doi, root, metadata_path, ms_dir) VALUES (?, ?, ?, ?) ON CONFLICT(doi, root) DO UPDATE SET metadata_path = excluded.metadata_path, ms_dir = excluded.ms_dir")
            .bind::<Text, _>(&options.doi)
            .bind::<Text, _>(text(&root)?)
            .bind::<Text, _>(text(&metadata)?)
            .bind::<Text, _>(text(&ms_dir)?)
            .execute(conn)?;
        let id = diesel::sql_query("SELECT id FROM datasets WHERE doi = ? AND root = ?")
            .bind::<Text, _>(&options.doi)
            .bind::<Text, _>(text(&root)?)
            .get_result::<DatasetId>(conn)?.id;
        let preserved = diesel::sql_query("SELECT p.filename, s.relative_path, p.ms2_path, p.ms2_sha256, p.features_path, p.paired_path, p.raw_identity, p.registered_at FROM processed_spectra p JOIN spectrum_files s USING (dataset_id, filename) WHERE p.dataset_id = ?")
            .bind::<BigInt, _>(id)
            .load::<PreservedSpectrum>(conn)?;
        diesel::sql_query("DELETE FROM sample_rows WHERE dataset_id = ?")
            .bind::<BigInt, _>(id).execute(conn)?;
        let mut rows = 0u64;
        let mut spectra = 0u64;
        let mut taxon_ids = HashMap::<String, i64>::new();
        for record in csv.records().take(options.max_rows.unwrap_or(usize::MAX)) {
            let record = record?;
            rows += 1;
            let row_number = i64::try_from(rows).context("too many sample rows")?;
            let taxon_id = if let Some(name) = record
                .get(taxon_index)
                .map(str::trim)
                .filter(|name| !name.is_empty())
            {
                if let Some(&id) = taxon_ids.get(name) {
                    Some(id)
                } else {
                    diesel::sql_query("INSERT INTO taxa (name) VALUES (?) ON CONFLICT(name) DO NOTHING")
                        .bind::<Text, _>(name)
                        .execute(conn)?;
                    let id = diesel::sql_query("SELECT id FROM taxa WHERE name = ?")
                        .bind::<Text, _>(name)
                        .get_result::<DatasetId>(conn)?.id;
                    taxon_ids.insert(name.to_owned(), id);
                    Some(id)
                }
            } else {
                None
            };
            let attributes: Map<String, Value> = headers.iter().zip(record.iter())
                .map(|(name, value)| (name.to_owned(), Value::String(value.to_owned()))).collect();
            let json = serde_json::to_string(&attributes)?;
            diesel::sql_query("INSERT INTO sample_rows (dataset_id, row_number, sample_id, taxon_id, attributes_json) VALUES (?, ?, ?, ?, ?)")
                .bind::<BigInt, _>(id)
                .bind::<BigInt, _>(row_number)
                .bind::<diesel::sql_types::Nullable<Text>, _>(sample_id_index
                    .and_then(|i| record.get(i))
                    .filter(|sample| !sample.trim().is_empty()))
                .bind::<diesel::sql_types::Nullable<BigInt>, _>(taxon_id)
                .bind::<Text, _>(&json)
                .execute(conn)?;
            for &index in &checked.filename_columns {
                let filename = record.get(index).context("missing filename column")?.trim();
                if filename.is_empty() { continue; }
                let file = checked.files.get(filename).with_context(|| format!("unvalidated spectrum {filename:?}"))?;
                let relative = file.strip_prefix(&root).context("spectrum outside dataset")?;
                diesel::sql_query("INSERT INTO spectrum_files (dataset_id, filename, row_number, column_name, relative_path) VALUES (?, ?, ?, ?, ?)")
                    .bind::<BigInt, _>(id)
                    .bind::<Text, _>(filename)
                    .bind::<BigInt, _>(row_number)
                    .bind::<Text, _>(headers.get(index).context("missing column header")?)
                    .bind::<Text, _>(text(relative)?)
                    .execute(conn)?;
                spectra += 1;
                if options.verbose { eprintln!("imported {}", relative.display()); }
            }
            progress.inc(1);
        }
        if spectra != checked.files.len() as u64 {
            bail!("spectrum inventory changed since validation");
        }
        for old in preserved {
            let Some(file) = checked.files.get(&old.filename) else { continue };
            let Ok(relative) = file.strip_prefix(&root) else { continue };
            if relative != Path::new(&old.relative_path)
                || register::raw_identity(file).ok().as_deref() != Some(&old.raw_identity)
            {
                continue;
            }
            diesel::sql_query("INSERT INTO processed_spectra (dataset_id, filename, ms2_path, ms2_sha256, features_path, paired_path, registered_at, raw_identity) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
                .bind::<BigInt, _>(id)
                .bind::<Text, _>(&old.filename)
                .bind::<Text, _>(&old.ms2_path)
                .bind::<Text, _>(&old.ms2_sha256)
                .bind::<Text, _>(&old.features_path)
                .bind::<diesel::sql_types::Nullable<Text>, _>(old.paired_path.as_deref())
                .bind::<diesel::sql_types::Nullable<Text>, _>(old.registered_at.as_deref())
                .bind::<Text, _>(&old.raw_identity)
                .execute(conn)?;
        }
        diesel::sql_query(
            "DELETE FROM taxa WHERE NOT EXISTS (SELECT 1 FROM sample_rows WHERE taxon_id = taxa.id)",
        )
        .execute(conn)?;
        Ok(Counts { rows, spectra })
    });
    progress.finish_and_clear();
    result
}
