use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use peregrino::annotations::{AnnotationCommand, run as run_annotations};
use peregrino::inspect::{list_tables, write_table};
use peregrino::register::{RegisterOptions, register};
use peregrino::{IngestOptions, ingest, metadata_headers};

#[derive(Parser)]
#[command(about = "Validated metadata ingestion, picoto registration, and SQLite inspection")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate via machaca, then atomically import samples and spectrum paths.
    Ingest {
        dataset: PathBuf,
        #[arg(long)]
        doi: String,
        #[arg(long)]
        metadata: PathBuf,
        #[arg(long)]
        ms_dir: PathBuf,
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        db: PathBuf,
        #[arg(long)]
        verbose: bool,
        /// Validate and import only the first N metadata rows.
        #[arg(long)]
        max_rows: Option<usize>,
    },
    /// Link picoto outputs to a previously ingested raw filename.
    Register {
        #[arg(long)]
        db: PathBuf,
        #[arg(long)]
        dataset: PathBuf,
        #[arg(long)]
        raw: String,
        #[arg(long)]
        ms2: PathBuf,
        #[arg(long)]
        features: PathBuf,
        #[arg(long)]
        paired: Option<PathBuf>,
    },
    /// List user tables and views in an existing database.
    Tables {
        #[arg(long)]
        db: PathBuf,
    },
    /// Preview a table or view as tab-separated rows on stdout.
    Show {
        table: String,
        #[arg(long)]
        db: PathBuf,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Export every row of a table or view to a new TSV file.
    Export {
        table: String,
        #[arg(long)]
        db: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Explore current Hocofaisan annotations as TSV without modifying the database.
    Annotations {
        #[arg(long)]
        db: PathBuf,
        #[command(subcommand)]
        command: AnnotationCommand,
    },
}

fn taxon_column(config: &Path, dataset: &Path, metadata: &Path) -> anyhow::Result<String> {
    match fs::read_to_string(config) {
        Ok(contents) => {
            let parsed: toml::Value = contents
                .parse()
                .with_context(|| format!("parse config {}", config.display()))?;
            let column = parsed
                .get("dataset")
                .and_then(|dataset| dataset.get("taxon_column"))
                .and_then(toml::Value::as_str)
                .filter(|column| !column.is_empty())
                .with_context(|| {
                    format!(
                        "{} must contain a nonempty [dataset] taxon_column string",
                        config.display()
                    )
                })?;
            Ok(column.to_owned())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let headers = metadata_headers(dataset, metadata)?;
            if headers.is_empty()
                || headers.iter().any(str::is_empty)
                || headers.iter().collect::<std::collections::HashSet<_>>().len() != headers.len()
            {
                bail!("metadata headers must be nonempty and unique");
            }
            eprintln!("Select the metadata taxon column:");
            for (index, header) in headers.iter().enumerate() {
                eprintln!("  {}. {}", index + 1, header);
            }
            eprint!("Column number: ");
            io::stderr().flush()?;
            let mut choice = String::new();
            if io::stdin().read_line(&mut choice)? == 0 {
                bail!("no taxon column selected (input ended)");
            }
            let index = choice
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|number| *number > 0 && *number <= headers.len())
                .with_context(|| format!("invalid taxon column selection: {:?}", choice.trim()))?
                - 1;
            let column = headers.get(index).context("selected metadata header is missing")?;
            let mut document = toml::Table::new();
            let mut dataset_config = toml::Table::new();
            dataset_config.insert("taxon_column".into(), toml::Value::String(column.into()));
            document.insert("dataset".into(), toml::Value::Table(dataset_config));
            let parent = config
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            fs::create_dir_all(parent)
                .with_context(|| format!("create config directory {}", parent.display()))?;
            let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
            temporary.write_all(toml::to_string(&document)?.as_bytes())?;
            temporary.persist_noclobber(config).with_context(|| {
                format!("create config {} without overwriting an existing file", config.display())
            })?;
            Ok(column.to_owned())
        }
        Err(error) => Err(error).with_context(|| format!("read config {}", config.display())),
    }
}

fn main() -> anyhow::Result<()> {
    let Cli { command } = Cli::parse();
    match command {
        Command::Ingest {
            dataset,
            doi,
            metadata,
            ms_dir,
            config,
            db,
            verbose,
            max_rows,
        } => {
            let taxon_column = taxon_column(&config, &dataset, &metadata)?;
            let counts = ingest(&IngestOptions {
                root: dataset,
                doi,
                metadata,
                taxon_column,
                ms_dir,
                database: db,
                verbose,
                max_rows,
            })?;
            println!(
                "Imported {} sample rows and {} spectra",
                counts.rows, counts.spectra
            );
        }
        Command::Register { db, dataset, raw, ms2, features, paired } => {
            register(&RegisterOptions { database: db, dataset, raw, ms2, features, paired })?;
            println!("Registered picoto spectra");
        }
        Command::Tables { db } => list_tables(&db, std::io::stdout().lock())?,
        Command::Show { table, db, limit } => {
            write_table(&db, &table, Some(limit), std::io::stdout().lock())?;
        }
        Command::Annotations { db, command } => {
            run_annotations(&db, command, std::io::stdout().lock())?;
        }
        Command::Export { table, db, out } => {
            let parent = out
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            std::fs::create_dir_all(parent)?;
            let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
            write_table(&db, &table, None, tmp.as_file_mut())?;
            tmp.as_file_mut().flush()?;
            tmp.persist_noclobber(&out)?;
            println!("Exported {table} to {}", out.display());
        }
    }
    Ok(())
}
