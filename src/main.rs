use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use peregrino::inspect::{list_tables, write_table};
use peregrino::register::{RegisterOptions, register};
use peregrino::{IngestOptions, ingest};

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
        db: PathBuf,
        #[arg(long)]
        verbose: bool,
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
}

fn main() -> anyhow::Result<()> {
    let Cli { command } = Cli::parse();
    match command {
        Command::Ingest {
            dataset,
            doi,
            metadata,
            ms_dir,
            db,
            verbose,
        } => {
            let counts = ingest(&IngestOptions {
                root: dataset,
                doi,
                metadata,
                ms_dir,
                database: db,
                verbose,
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
