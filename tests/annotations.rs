use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rusqlite::Connection;

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("annotations.sqlite");
    let db = Connection::open(&path).unwrap();
    db.execute_batch(
        "CREATE TABLE spectrum_files(dataset_id INTEGER, filename TEXT, PRIMARY KEY(dataset_id,filename));
         CREATE TABLE processed_spectra(dataset_id INTEGER, filename TEXT, ms2_sha256 TEXT, PRIMARY KEY(dataset_id,filename));
         CREATE TABLE annotation_runs(
             id INTEGER PRIMARY KEY, dataset_id INTEGER, filename TEXT,
             ms2_sha256 TEXT, registered_ms2_sha256 TEXT,
             status TEXT, error TEXT, taxon_name TEXT, taxon_qid TEXT,
             query_count INTEGER, hit_count INTEGER, created_at TEXT
         );
         CREATE TABLE annotation_features(
             run_id INTEGER, feature_id TEXT, precursor_mz REAL, hit_count INTEGER,
             status TEXT, PRIMARY KEY(run_id,feature_id)
         );
         CREATE TABLE annotation_hits(
             run_id INTEGER, feature_id TEXT, rank INTEGER, rank_before_taxonomy INTEGER,
             library_spectrum_id TEXT, library_name TEXT, library_inchikey TEXT,
             spectral_score REAL, taxonomic_score REAL, combined_score REAL,
             taxonomic_support TEXT, matches INTEGER, ms1_deviation_ppm REAL,
             matched_organism_name TEXT, matched_organism_wikidata TEXT,
             matched_shared_rank TEXT, matched_short_inchikey TEXT,
             PRIMARY KEY(run_id,feature_id,rank)
         );
         CREATE VIEW annotation_current_runs AS
           SELECT * FROM (
             SELECT r.*, ROW_NUMBER() OVER (
               PARTITION BY r.dataset_id,r.filename ORDER BY r.id DESC
             ) AS latest_rank
             FROM annotation_runs r
             JOIN processed_spectra p USING(dataset_id,filename)
             WHERE r.ms2_sha256=p.ms2_sha256
               AND r.registered_ms2_sha256=p.ms2_sha256
           ) WHERE latest_rank=1;
         INSERT INTO spectrum_files VALUES
           (1,'a.mzML'),(1,'b.mzML'),(1,'c.mzML'),(1,'d.mzML'),
           (1,'e.mzML'),(2,'a.mzML');
         INSERT INTO processed_spectra VALUES
           (1,'a.mzML','a'),(1,'b.mzML','b'),(1,'c.mzML','c'),
           (1,'e.mzML','new'),(2,'a.mzML','z');
         INSERT INTO annotation_runs VALUES
           (1,1,'a.mzML','a','a','hits',NULL,'Plant A','Q1',1,1,'2026-01-01'),
           (2,1,'a.mzML','a','a','hits',NULL,'Plant A','Q1',2,2,'2026-01-02'),
           (3,1,'b.mzML','b','b','error','Biosource not found in LOTUS','Plant B','Q2',0,0,'2026-01-03'),
           (4,1,'e.mzML','old','old','hits',NULL,'Plant E','Q5',1,1,'2026-01-04'),
           (5,2,'a.mzML','z','z','no_hits',NULL,'Plant Z','Q9',1,0,'2026-01-05');
         INSERT INTO annotation_features VALUES
           (1,'old',400,1,'hits'),(2,'f1',400,2,'hits'),(2,'f2',450,0,'no_hits'),
           (4,'stale',500,1,'hits'),(5,'z1',500,0,'no_hits');
         INSERT INTO annotation_hits VALUES
           (1,'old',1,1,'previous','Old','OLD',1,1,1,'supported',10,0,'Old','Q1','species','OLD'),
           (2,'f1',1,2,'supported-hit','Molecule A','KEY-A',0.75,0.2,0.85,'supported',9,3,'Plant A','Q1','species','KEY-A'),
           (2,'f1',2,1,'spectral-hit','Molecule B','KEY-B',0.90,0,0.72,'unsupported',8,4,'Other','Q10','family','KEY-B'),
           (4,'stale',1,1,'obsolete','Old','OLD',1,1,1,'supported',10,0,'Old','Q5','species','OLD');",
    )
    .unwrap();
    drop(db);
    (dir, path)
}

fn cli(db: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_peregrino"))
        .args(["annotations", "--db", db.to_str().unwrap()])
        .args(args)
        .output()
        .unwrap()
}

fn records(output: Output) -> Vec<HashMap<String, String>> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b'\t')
        .from_reader(output.stdout.as_slice());
    let headers = reader.headers().unwrap().clone();
    reader
        .records()
        .map(|record| {
            headers
                .iter()
                .zip(record.unwrap().iter())
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect()
        })
        .collect()
}

#[test]
fn counts_only_latest_current_results_without_losing_unannotated_files() {
    let (_dir, db) = fixture();
    let rows = records(cli(&db, &["summary"]));
    let by_status = rows
        .iter()
        .map(|row| (row["status"].as_str(), row["files"].as_str()))
        .collect::<HashMap<_, _>>();
    assert_eq!(by_status["hits"], "1");
    assert_eq!(by_status["no_hits"], "1");
    assert_eq!(by_status["error"], "1");
    assert_eq!(by_status["not_annotated"], "2");
    assert_eq!(by_status["unregistered"], "1");
    let dataset = records(cli(&db, &["summary", "--dataset-id", "2"]));
    assert_eq!(
        dataset
            .iter()
            .find(|row| row["status"] == "no_hits")
            .unwrap()["files"],
        "1"
    );
    assert_eq!(
        dataset.iter().find(|row| row["status"] == "hits").unwrap()["files"],
        "0"
    );

    let rows = records(cli(
        &db,
        &["files", "--dataset-id", "1", "--status", "hits"],
    ));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["filename"], "a.mzML");
    assert_eq!(rows[0]["hit_count"], "2");
    assert_eq!(rows[0]["query_count"], "2");
}

#[test]
fn explores_zero_hit_features_and_reranked_score_filters() {
    let (_dir, db) = fixture();
    let features = records(cli(
        &db,
        &["features", "--file", "a.mzML", "--dataset-id", "1"],
    ));
    assert_eq!(features.len(), 2);
    let zero_hit = features
        .iter()
        .find(|row| row["feature_id"] == "f2")
        .unwrap();
    assert_eq!(zero_hit["hit_count"], "0");

    let spectral = records(cli(
        &db,
        &[
            "hits",
            "--file",
            "a.mzML",
            "--dataset-id",
            "1",
            "--min-spectral-score",
            "0.8",
        ],
    ));
    assert_eq!(spectral.len(), 1);
    assert_eq!(spectral[0]["library_spectrum_id"], "spectral-hit");
    assert_eq!(spectral[0]["taxonomic_score"], "0");

    let supported = records(cli(
        &db,
        &[
            "hits",
            "--file",
            "a.mzML",
            "--dataset-id",
            "1",
            "--min-taxonomic-score",
            "0.1",
            "--support",
            "supported",
            "--shared-rank",
            "species",
        ],
    ));
    assert_eq!(supported.len(), 1);
    assert_eq!(supported[0]["library_spectrum_id"], "supported-hit");
    assert_eq!(supported[0]["rank_before_taxonomy"], "2");
    assert_eq!(supported[0]["combined_score"], "0.85");
}

#[test]
fn counts_filtered_hits_per_file_without_confusing_hits_and_features() {
    let (_dir, db) = fixture();
    assert!(!cli(&db, &["counts"]).status.success());

    let rows = records(cli(
        &db,
        &[
            "counts",
            "--dataset-id",
            "1",
            "--file",
            "a.mzML",
            "--min-spectral-score",
            "0.9",
        ],
    ));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["total_hits"], "2");
    assert_eq!(rows[0]["matching_hits"], "1");
    assert_eq!(rows[0]["features_with_matching_hits"], "1");
    assert_eq!(rows[0]["max_taxonomic_score"], "0");

    let species = records(cli(
        &db,
        &[
            "counts",
            "--dataset-id",
            "1",
            "--file",
            "a.mzML",
            "--min-taxonomic-score",
            "0.2",
            "--support",
            "supported",
            "--shared-rank",
            "species",
            "--min-matches",
            "9",
        ],
    ));
    assert_eq!(species[0]["matching_hits"], "1");
    assert_eq!(species[0]["max_taxonomic_score"], "0.2");

    let no_hits = records(cli(&db, &["counts", "--dataset-id", "2"]));
    assert_eq!(no_hits.len(), 1);
    assert_eq!(no_hits[0]["matching_hits"], "0");
}

#[test]
fn reports_errors_without_old_or_stale_runs_and_never_creates_a_missing_db() {
    let (dir, db) = fixture();
    let errors = records(cli(&db, &["errors", "--dataset-id", "1"]));
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["filename"], "b.mzML");
    assert!(errors[0]["error"].contains("LOTUS"));

    let missing = dir.path().join("missing.sqlite");
    assert!(!cli(&missing, &["summary"]).status.success());
    assert!(!missing.exists());

    let empty = dir.path().join("empty.sqlite");
    Connection::open(&empty).unwrap();
    let result = cli(&empty, &["summary"]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("annotation"));
}
