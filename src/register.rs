//! Validate picoto outputs and register them against an already-ingested raw spectrum.
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use diesel::connection::SimpleConnection;
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Text};
use sha2::{Digest, Sha256};

use crate::{DatasetId, text};

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

fn accepted_features(path: &Path) -> Result<HashSet<String>> {
    let mut table = csv::ReaderBuilder::new().delimiter(b'\t').from_path(path)?;
    let headers = table.headers()?;
    const HEADERS: [&str; 14] = ["feature_id", "mz", "rt_seconds", "apex_intensity", "apex_to_baseline", "ms1_scans", "area", "isotope_peaks", "quality_score", "status", "reason", "ms2_scan_ids", "ms2_quality", "collision_energy"];
    ensure!(headers.iter().eq(HEADERS), "features must be a picoto audit TSV");
    let mut accepted = HashSet::new();
    for record in table.records() {
        let record = record?;
        let id = record.get(0).context("missing feature ID")?;
        match record.get(9).context("missing feature status")? {
            "accepted" => {
                ensure!(!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) && accepted.insert(id.to_owned()), "duplicate or invalid accepted audit feature ID {id:?}");
            }
            "rejected" => ensure!(id.is_empty(), "rejected picoto features have no feature ID"),
            status => bail!("invalid audit status {status:?}"),
        }
    }
    Ok(accepted)
}

fn validate_mgf(path: &Path, raw: &str, expected: &HashSet<String>, paired: bool) -> Result<()> {
    let mut seen_ms1 = HashSet::new();
    let mut seen_ms2 = HashSet::new();
    let mut block = false;
    let mut id = None::<String>;
    let mut level = None::<u8>;
    let mut filename = None::<String>;
    let mut title = None::<String>;
    let mut pepmass = false;
    let mut charge = false;
    let mut rt = false;
    let mut peaks = false;
    let mut line = String::new();
    let mut reader = BufReader::new(File::open(path)?);
    while reader.read_line(&mut line)? > 0 {
        let entry = line.trim();
        if entry == "BEGIN IONS" {
            ensure!(!block, "nested MGF blocks in {}", path.display());
            block = true;
            id = None;
            level = None;
            filename = None;
            title = None;
            pepmass = false;
            charge = false;
            rt = false;
            peaks = false;
        } else if entry == "END IONS" {
            ensure!(block, "MGF END IONS without BEGIN IONS");
            let feature = id.as_deref().context("MGF block missing FEATURE_ID")?;
            let ms_level = level.context("MGF block missing MSLEVEL")?;
            let title_matches = title.as_deref()
                .and_then(|value| value.strip_prefix(raw))
                .and_then(|value| value.strip_prefix(":feature_"))
                .and_then(|value| value.strip_prefix(feature))
                .is_some_and(|value| value == match ms_level { 1 => ":MS1", 2 => ":MS2", _ => "" });
            ensure!(filename.as_deref() == Some(raw) && title_matches, "MGF FILENAME/TITLE do not match registered raw file");
            ensure!(pepmass && charge && rt && peaks, "MGF block missing picoto precursor/charge/RT/peaks");
            ensure!(expected.contains(feature), "MGF feature {feature} is not accepted in audit");
            let seen = match ms_level {
                1 if paired => &mut seen_ms1,
                2 => &mut seen_ms2,
                _ => bail!("unsupported MSLEVEL {ms_level} in {}", path.display()),
            };
            ensure!(seen.insert(feature.to_owned()), "duplicate FEATURE_ID {feature} at MSLEVEL {ms_level}");
            block = false;
        } else if !entry.is_empty() {
            ensure!(block, "content outside MGF block in {}", path.display());
            if let Some((key, value)) = entry.split_once('=') {
                match key {
                    "FEATURE_ID" => { ensure!(id.replace(value.to_owned()).is_none() && !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()), "invalid or repeated FEATURE_ID"); }
                    "MSLEVEL" => { ensure!(level.replace(value.parse().context("invalid MSLEVEL")?).is_none(), "repeated MSLEVEL"); }
                    "FILENAME" => { ensure!(filename.replace(value.to_owned()).is_none(), "repeated FILENAME"); }
                    "TITLE" => { ensure!(title.replace(value.to_owned()).is_none(), "repeated TITLE"); }
                    "PEPMASS" => { ensure!(value.parse::<f64>().is_ok_and(|n| n.is_finite() && n > 0.0), "invalid PEPMASS"); pepmass = true; }
                    "CHARGE" => { ensure!(value.ends_with('+') || value.ends_with('-'), "invalid CHARGE"); charge = true; }
                    "RTINSECONDS" => { ensure!(value.parse::<f64>().is_ok_and(|n| n.is_finite() && n >= 0.0), "invalid RTINSECONDS"); rt = true; }
                    _ => {}
                }
            } else {
                let mut values = entry.split_whitespace();
                let mz = values.next().and_then(|x| x.parse::<f64>().ok());
                let intensity = values.next().and_then(|x| x.parse::<f64>().ok());
                ensure!(values.next().is_none() && mz.is_some_and(|x| x.is_finite() && x > 0.0) && intensity.is_some_and(|x| x.is_finite() && x >= 0.0), "invalid MGF peak {entry:?}");
                peaks = true;
            }
        }
        line.clear();
    }
    ensure!(!block, "unclosed MGF block in {}", path.display());
    ensure!(seen_ms2 == *expected && (!paired || seen_ms1 == *expected), "MGF FEATURE_IDs do not match accepted audit features");
    Ok(())
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
    let expected = accepted_features(&features)?;
    validate_mgf(&ms2, &options.raw, &expected, false)?;
    if let Some(paired) = &paired { validate_mgf(paired, &options.raw, &expected, true)?; }
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
    db.batch_execute("PRAGMA foreign_keys = ON;")?;
    db.transaction::<(), anyhow::Error, _>(|conn| {
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
