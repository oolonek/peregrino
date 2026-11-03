//! Read-only, current-MS2 annotation reports over the Peregrino database.
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::{Subcommand, ValueEnum};
use rusqlite::Connection;

use crate::inspect::{open, write_query};

#[derive(Clone, Copy, ValueEnum)]
pub enum FileStatus {
    Hits,
    #[value(name = "no_hits")]
    NoHits,
    Error,
    #[value(name = "not_annotated")]
    NotAnnotated,
    Unregistered,
}

impl FileStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Hits => "hits",
            Self::NoHits => "no_hits",
            Self::Error => "error",
            Self::NotAnnotated => "not_annotated",
            Self::Unregistered => "unregistered",
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Support {
    Supported,
    Unsupported,
}

impl Support {
    fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Subcommand)]
pub enum AnnotationCommand {
    /// Count ingested spectrum files by their current annotation status.
    Summary {
        #[arg(long)]
        dataset_id: Option<i64>,
    },
    /// List ingested files, including registrations without current annotations.
    Files {
        #[arg(long)]
        dataset_id: Option<i64>,
        #[arg(long)]
        status: Option<FileStatus>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Count retained hits per current file after score and taxonomy filters.
    Counts {
        #[arg(long)]
        dataset_id: Option<i64>,
        #[arg(long)]
        file: Option<String>,
        #[arg(long)]
        min_spectral_score: Option<f64>,
        #[arg(long)]
        min_taxonomic_score: Option<f64>,
        #[arg(long)]
        support: Option<Support>,
        #[arg(long)]
        shared_rank: Option<String>,
        #[arg(long)]
        min_matches: Option<i64>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// List annotated features, including those without hits, for an exact filename.
    Features {
        #[arg(long)]
        file: String,
        #[arg(long)]
        dataset_id: Option<i64>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// List scored hits for an exact filename, optionally narrowed to one feature.
    Hits {
        #[arg(long)]
        file: String,
        #[arg(long)]
        feature: Option<String>,
        #[arg(long)]
        dataset_id: Option<i64>,
        #[arg(long)]
        min_spectral_score: Option<f64>,
        #[arg(long)]
        min_taxonomic_score: Option<f64>,
        #[arg(long)]
        support: Option<Support>,
        #[arg(long)]
        shared_rank: Option<String>,
        #[arg(long)]
        min_matches: Option<i64>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// List errors from current annotation runs only.
    Errors {
        #[arg(long)]
        dataset_id: Option<i64>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

fn require_schema(db: &Connection) -> Result<()> {
    let present: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE (type = 'table' AND name IN \
         ('spectrum_files', 'processed_spectra', 'annotation_runs', 'annotation_features', 'annotation_hits')) \
         OR (type = 'view' AND name = 'annotation_current_runs')",
        [],
        |row| row.get(0),
    )?;
    if present != 6 {
        bail!("annotation schema is absent or incomplete in this database; run hocofaisan first");
    }
    Ok(())
}

fn limit(value: usize) -> Result<i64> {
    i64::try_from(value).context("--limit is too large for SQLite")
}

// Status is derived from *all* ingested files. No historical or stale annotation
// run is substituted when the registered MS2 has changed.
const FILE_STATUS: &str = "
WITH file_status AS (
    SELECT f.dataset_id, f.filename,
           CASE WHEN p.dataset_id IS NULL THEN 'unregistered'
                WHEN r.id IS NULL THEN 'not_annotated'
                ELSE r.status END AS status,
           r.taxon_name, r.taxon_qid,
           COALESCE(r.query_count, 0) AS query_count,
           COALESCE(r.hit_count, 0) AS hit_count
    FROM spectrum_files AS f
    LEFT JOIN processed_spectra AS p
      ON p.dataset_id = f.dataset_id AND p.filename = f.filename
    LEFT JOIN annotation_current_runs AS r
      ON r.dataset_id = f.dataset_id AND r.filename = f.filename
)
";

/// Write a TSV report without modifying (or creating) the database.
pub fn run(database: &Path, command: AnnotationCommand, output: impl Write) -> Result<()> {
    let db = open(database)?;
    require_schema(&db)?;
    match command {
        AnnotationCommand::Summary { dataset_id } => write_query(
            &db,
            &format!(
                "{FILE_STATUS}
                 SELECT s.status, count(f.status) AS files,
                        COALESCE(sum(f.query_count), 0) AS query_count,
                        COALESCE(sum(f.hit_count), 0) AS hit_count
                 FROM (SELECT 'hits' AS status UNION ALL SELECT 'no_hits'
                       UNION ALL SELECT 'error' UNION ALL SELECT 'not_annotated'
                       UNION ALL SELECT 'unregistered') AS s
                 LEFT JOIN file_status AS f ON f.status = s.status
                   AND (?1 IS NULL OR f.dataset_id = ?1)
                 GROUP BY s.status
                 ORDER BY CASE s.status WHEN 'hits' THEN 1 WHEN 'no_hits' THEN 2
                          WHEN 'error' THEN 3 WHEN 'not_annotated' THEN 4 ELSE 5 END"
            ),
            &[&dataset_id],
            output,
        ),
        AnnotationCommand::Files {
            dataset_id,
            status,
            limit: max,
        } => {
            let status = status.map(FileStatus::as_str);
            let max = limit(max)?;
            write_query(
                &db,
                &format!(
                    "{FILE_STATUS}
                     SELECT dataset_id, filename, status, taxon_name, taxon_qid,
                            query_count, hit_count
                     FROM file_status
                     WHERE (?1 IS NULL OR dataset_id = ?1)
                       AND (?2 IS NULL OR status = ?2)
                     ORDER BY hit_count DESC, dataset_id, filename
                     LIMIT ?3"
                ),
                &[&dataset_id, &status, &max],
                output,
            )
        }
        AnnotationCommand::Counts {
            dataset_id,
            file,
            min_spectral_score,
            min_taxonomic_score,
            support,
            shared_rank,
            min_matches,
            limit: max,
        } => {
            if dataset_id.is_none() && file.is_none() {
                bail!("counts requires --dataset-id or --file to bound the hit scan");
            }
            let support = support.map(Support::as_str);
            let max = limit(max)?;
            write_query(
                &db,
                "SELECT r.dataset_id, r.filename, r.taxon_name, r.query_count,
                        r.hit_count AS total_hits,
                        COUNT(h.rank) AS matching_hits,
                        COUNT(DISTINCT h.feature_id) AS features_with_matching_hits,
                        MIN(h.spectral_score) AS min_spectral_score,
                        MAX(h.spectral_score) AS max_spectral_score,
                        MAX(h.taxonomic_score) AS max_taxonomic_score
                 FROM annotation_current_runs AS r
                 LEFT JOIN annotation_hits AS h ON h.run_id = r.id
                   AND (?3 IS NULL OR h.spectral_score >= ?3)
                   AND (?4 IS NULL OR h.taxonomic_score >= ?4)
                   AND (?5 IS NULL OR h.taxonomic_support = ?5)
                   AND (?6 IS NULL OR h.matched_shared_rank = ?6)
                   AND (?7 IS NULL OR h.matches >= ?7)
                 WHERE r.status IN ('hits', 'no_hits')
                   AND (?1 IS NULL OR r.dataset_id = ?1)
                   AND (?2 IS NULL OR r.filename = ?2)
                 GROUP BY r.id
                 ORDER BY matching_hits DESC, r.dataset_id, r.filename
                 LIMIT ?8",
                &[
                    &dataset_id,
                    &file,
                    &min_spectral_score,
                    &min_taxonomic_score,
                    &support,
                    &shared_rank,
                    &min_matches,
                    &max,
                ],
                output,
            )
        }
        AnnotationCommand::Features {
            file,
            dataset_id,
            limit: max,
        } => {
            let max = limit(max)?;
            write_query(
                &db,
                "SELECT r.dataset_id, r.filename, f.feature_id, f.precursor_mz,
                        f.hit_count, h.combined_score AS rank1_combined_score,
                        h.spectral_score AS rank1_spectral_score,
                        h.taxonomic_score AS rank1_taxonomic_score,
                        h.matched_organism_name AS rank1_organism_name
                 FROM annotation_current_runs AS r
                 JOIN annotation_features AS f ON f.run_id = r.id
                 LEFT JOIN annotation_hits AS h
                   ON h.run_id = f.run_id AND h.feature_id = f.feature_id AND h.rank = 1
                 WHERE r.filename = ?1 AND (?2 IS NULL OR r.dataset_id = ?2)
                   AND r.status IN ('hits', 'no_hits')
                 ORDER BY f.hit_count DESC, r.dataset_id, f.feature_id
                 LIMIT ?3",
                &[&file, &dataset_id, &max],
                output,
            )
        }
        AnnotationCommand::Hits {
            file,
            feature,
            dataset_id,
            min_spectral_score,
            min_taxonomic_score,
            support,
            shared_rank,
            min_matches,
            limit: max,
        } => {
            let support = support.map(Support::as_str);
            let max = limit(max)?;
            write_query(
                &db,
                "SELECT r.dataset_id, r.filename, h.feature_id,
                        h.rank_before_taxonomy, h.rank AS rank_after_taxonomy,
                        h.library_spectrum_id, h.library_name, h.library_inchikey,
                        h.spectral_score, h.taxonomic_score, h.combined_score,
                        h.matches, h.ms1_deviation_ppm, h.taxonomic_support,
                        h.matched_shared_rank, h.matched_organism_name,
                        h.matched_organism_wikidata, h.matched_short_inchikey
                 FROM annotation_current_runs AS r
                 JOIN annotation_hits AS h ON h.run_id = r.id
                 WHERE r.filename = ?1 AND (?2 IS NULL OR r.dataset_id = ?2)
                   AND (?3 IS NULL OR h.feature_id = ?3)
                   AND (?4 IS NULL OR h.spectral_score >= ?4)
                   AND (?5 IS NULL OR h.taxonomic_score >= ?5)
                   AND (?6 IS NULL OR h.taxonomic_support = ?6)
                   AND (?7 IS NULL OR h.matched_shared_rank = ?7)
                   AND (?8 IS NULL OR h.matches >= ?8)
                   AND r.status = 'hits'
                 ORDER BY h.combined_score DESC, h.spectral_score DESC,
                          r.dataset_id, h.feature_id, h.rank, h.library_spectrum_id
                 LIMIT ?9",
                &[
                    &file,
                    &dataset_id,
                    &feature,
                    &min_spectral_score,
                    &min_taxonomic_score,
                    &support,
                    &shared_rank,
                    &min_matches,
                    &max,
                ],
                output,
            )
        }
        AnnotationCommand::Errors {
            dataset_id,
            limit: max,
        } => {
            let max = limit(max)?;
            write_query(
                &db,
                "SELECT dataset_id, filename, taxon_name, taxon_qid, error, created_at
                 FROM annotation_current_runs
                 WHERE status = 'error' AND (?1 IS NULL OR dataset_id = ?1)
                 ORDER BY created_at DESC, dataset_id, filename
                 LIMIT ?2",
                &[&dataset_id, &max],
                output,
            )
        }
    }
}
