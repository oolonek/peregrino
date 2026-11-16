//! Validate picoto outputs and register them against an already-ingested raw spectrum.
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use diesel::connection::SimpleConnection;
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Text};
use sha2::{Digest, Sha256};

use crate::{DatasetId, artifact, text};

/// Paths supplied by the operator; the database and raw spectrum must already exist.
pub struct RegisterOptions {
    pub database: PathBuf,
    pub dataset: PathBuf,
    pub raw: String,
    pub ms2: PathBuf,
    pub features: PathBuf,
    pub paired: Option<PathBuf>,
}

#[derive(QueryableByName)]
struct RawSpectrum {
    #[diesel(sql_type = Text)]
    relative_path: String,
}

/// A lightweight identity for detecting changes to an ingested raw file without
/// rereading potentially multi-gigabyte source data at every ingestion.
pub(crate) fn raw_identity(path: &Path) -> Result<String> {
    let path = path.canonicalize().with_context(|| format!("raw spectrum {}", path.display()))?;
    let stat = fs::metadata(&path)?;
    ensure!(stat.is_file(), "raw spectrum must be a file");
    Ok(format!("{}:{}:{}:{}:{}:{}", path.display(), stat.dev(), stat.ino(), stat.len(), stat.mtime(), stat.mtime_nsec()))
}

fn file(path: &Path, name: &str) -> Result<PathBuf> {
    let path = path.canonicalize().with_context(|| format!("{name} {}", path.display()))?;
    ensure!(path.is_file(), "{name} must be a file: {}", path.display());
    Ok(path)
}

/// Register one picoto MS2 output and its audit (and optionally paired output).
/// Invalid input never modifies the registration or creates a database.
pub fn register(options: &RegisterOptions) -> Result<()> {
    ensure!(options.database.is_file(), "database does not exist: {}", options.database.display());
    let root = options.dataset.canonicalize().context("dataset root")?;
    ensure!(root.is_dir(), "dataset root must be a directory");
    ensure!(!options.raw.is_empty() && Path::new(&options.raw).file_name().is_some_and(|part| part == options.raw.as_str()), "raw must be a registered filename, not a path");
    let stem = Path::new(&options.raw).file_stem().and_then(|s| s.to_str()).context("raw filename has no UTF-8 stem")?;
    let ms2 = file(&options.ms2, "MS2 MGF")?;
    let features = file(&options.features, "feature audit")?;
    let paired = options.paired.as_deref().map(|path| file(path, "paired MGF")).transpose()?;
    let ms2_name = format!("{stem}_ms2.mgf");
    let features_name = format!("{stem}_features.tsv");
    ensure!(ms2.file_name().and_then(|name| name.to_str()) == Some(ms2_name.as_str()) && features.file_name().and_then(|name| name.to_str()) == Some(features_name.as_str()), "processed files must have picoto names matching raw filename");
    ensure!(features.parent() == ms2.parent(), "picoto outputs must share one directory");
    if let Some(paired) = &paired {
        let paired_name = format!("{stem}_sirius.mgf");
        ensure!(paired.file_name().and_then(|name| name.to_str()) == Some(paired_name.as_str()) && paired.parent() == ms2.parent(), "paired MGF must match the same picoto output");
    }
    let expected = artifact::audit(&features)?;
    artifact::validate_mgf(&ms2, &options.raw, &expected, false)?;
    if let Some(paired) = &paired { artifact::validate_mgf(paired, &options.raw, &expected, true)?; }
    let mut hasher = Sha256::new();
    let mut input = File::open(&ms2)?;
    let mut chunk = [0u8; 65536];
    loop {
        let count = input.read(&mut chunk)?;
        if count == 0 { break; }
        hasher.update(&chunk[..count]);
    }
    let digest = format!("{:x}", hasher.finalize());
    let mut db = SqliteConnection::establish(text(&options.database)?).context("open SQLite database")?;
    // File validation and hashing run concurrently; acquire the SQLite writer
    // reservation before reading rows to avoid competing read-to-write upgrades.
    db.batch_execute("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 30000;")?;
    db.immediate_transaction::<(), anyhow::Error, _>(|conn| {
        // Migrate existing ingested databases without creating a new one.
        conn.batch_execute(crate::PROCESSED_SCHEMA)?;
        let datasets = diesel::sql_query("SELECT id FROM datasets WHERE root = ?")
            .bind::<Text, _>(text(&root)?).load::<DatasetId>(conn)?;
        ensure!(datasets.len() == 1, "dataset root must identify exactly one ingested dataset");
        let dataset_id = datasets[0].id;
        let row = diesel::sql_query("SELECT relative_path FROM spectrum_files WHERE dataset_id = ? AND filename = ?")
            .bind::<BigInt, _>(dataset_id)
            .bind::<Text, _>(&options.raw)
            .get_result::<RawSpectrum>(conn).context("raw filename is not registered for dataset")?;
        let raw_path = root.join(&row.relative_path).canonicalize().context("registered raw spectrum is missing")?;
        ensure!(raw_path.starts_with(&root) && raw_path.is_file(), "registered raw spectrum is outside dataset or not a file");
        let identity = raw_identity(&raw_path)?;
        diesel::sql_query("INSERT INTO processed_spectra (dataset_id, filename, ms2_path, ms2_sha256, features_path, paired_path, raw_identity, registered_at) VALUES (?, ?, ?, ?, ?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) ON CONFLICT(dataset_id, filename) DO UPDATE SET ms2_path = excluded.ms2_path, ms2_sha256 = excluded.ms2_sha256, features_path = excluded.features_path, paired_path = excluded.paired_path, raw_identity = excluded.raw_identity, registered_at = excluded.registered_at")
            .bind::<BigInt, _>(dataset_id).bind::<Text, _>(&options.raw)
            .bind::<Text, _>(text(&ms2)?).bind::<Text, _>(&digest)
            .bind::<Text, _>(text(&features)?)
            .bind::<diesel::sql_types::Nullable<Text>, _>(paired.as_deref().map(text).transpose()?)
            .bind::<Text, _>(&identity).execute(conn)?;
        Ok(())
    })
}
