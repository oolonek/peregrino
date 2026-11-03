use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use diesel::prelude::*;
use diesel::sql_types::{BigInt, Text};

#[derive(QueryableByName, Debug, PartialEq, Eq)]
struct StoredSpectrum {
    #[diesel(sql_type = Text)]
    sample_id: String,
    #[diesel(sql_type = Text)]
    filename: String,
    #[diesel(sql_type = Text)]
    relative_path: String,
    #[diesel(sql_type = Text)]
    attributes_json: String,
}

#[derive(QueryableByName)]
struct Count {
    #[diesel(sql_type = BigInt)]
    count: i64,
}

fn dataset(root: &Path, contents: &str) {
    fs::create_dir_all(root.join("metadata")).unwrap();
    fs::create_dir_all(root.join("msdata/raw/nested")).unwrap();
    fs::write(root.join("metadata/metadata.tsv"), contents).unwrap();
    fs::write(root.join("taxon.toml"), "[dataset]\ntaxon_column = \"sample_id\"\n").unwrap();
    fs::write(root.join("msdata/raw/a.mzXML"), "spectrum").unwrap();
    fs::write(root.join("msdata/raw/nested/b.mzML"), "spectrum").unwrap();
}

fn ingest(root: &Path, db: &Path) -> Output {
    ingest_with_max_rows(root, db, None)
}

fn ingest_first(root: &Path, db: &Path, max_rows: usize) -> Output {
    ingest_with_max_rows(root, db, Some(max_rows))
}

fn ingest_with_max_rows(root: &Path, db: &Path, max_rows: Option<usize>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_peregrino"));
    command
        .args([
            "ingest",
            root.to_str().unwrap(),
            "--doi",
            "10.5281/zenodo.10018264",
        ])
        .args([
            "--metadata",
            "metadata/metadata.tsv",
            "--ms-dir",
            "msdata/raw",
            "--db",
        ])
        .arg(db)
        .args(["--config", root.join("taxon.toml").to_str().unwrap()]);
    if let Some(max_rows) = max_rows {
        command.args(["--max-rows", &max_rows.to_string()]);
    }
    command.output().unwrap()
}

fn stored(db: &Path) -> Vec<StoredSpectrum> {
    let mut conn = SqliteConnection::establish(db.to_str().unwrap()).unwrap();
    diesel::sql_query("SELECT sample_rows.sample_id, spectrum_files.filename, spectrum_files.relative_path, sample_rows.attributes_json FROM spectrum_files JOIN sample_rows USING (dataset_id, row_number) ORDER BY spectrum_files.filename")
        .load(&mut conn).unwrap()
}

#[test]
fn creates_missing_database_directory_after_validation() {
    let temp = tempfile::tempdir().unwrap();
    dataset(
        temp.path(),
        "sample_id\tpositive_filename\tnegative_filename\nA\ta.mzXML\tb.mzML\n",
    );
    let db = temp.path().join("new/database/location/flock.sqlite");
    let output = ingest(temp.path(), &db);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stored(&db).len(), 2);
}

#[test]
fn imports_validated_rows_with_nested_spectrum_paths_and_original_attributes() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("records.sqlite");
    dataset(
        temp.path(),
        "sample_id\tpositive\tnegative\torganism\nA\ta.mzXML\t\tPlantae\nB\t\tb.mzML\tFungi\n",
    );
    let output = ingest(temp.path(), &db);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("2 sample rows and 2 spectra"));
    let rows = stored(&db);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (
            &rows[0].sample_id,
            &rows[0].filename,
            &rows[0].relative_path
        ),
        (
            &"A".to_string(),
            &"a.mzXML".to_string(),
            &"msdata/raw/a.mzXML".to_string()
        )
    );
    assert_eq!(rows[1].relative_path, "msdata/raw/nested/b.mzML");
    let attributes: serde_json::Value = serde_json::from_str(&rows[1].attributes_json).unwrap();
    assert_eq!(attributes["organism"], "Fungi");
    assert_eq!(attributes["negative"], "b.mzML");
}

#[test]
fn reimport_replaces_rows_but_failed_validation_preserves_last_commit() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("records.sqlite");
    dataset(
        temp.path(),
        "sample_id\tpositive_filename\tnegative_filename\nA\ta.mzXML\t\nB\t\tb.mzML\n",
    );
    assert!(ingest(temp.path(), &db).status.success());
    fs::write(
        temp.path().join("metadata/metadata.tsv"),
        "sample_id\tpositive_filename\tnegative_filename\nC\ta.mzXML\tb.mzML\n",
    )
    .unwrap();
    assert!(ingest(temp.path(), &db).status.success());
    let mut conn = SqliteConnection::establish(db.to_str().unwrap()).unwrap();
    let count: Count = diesel::sql_query("SELECT COUNT(*) AS count FROM sample_rows")
        .get_result(&mut conn)
        .unwrap();
    assert_eq!(count.count, 1);
    assert_eq!(
        stored(&db)
            .iter()
            .map(|r| r.sample_id.as_str())
            .collect::<Vec<_>>(),
        ["C", "C"]
    );
    fs::write(
        temp.path().join("metadata/metadata.tsv"),
        "sample_id\tpositive_filename\tnegative_filename\nD\tmissing.mzXML\tb.mzML\n",
    )
    .unwrap();
    let failed = ingest(temp.path(), &db);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("missing file"));
    assert_eq!(
        stored(&db)
            .iter()
            .map(|r| r.sample_id.as_str())
            .collect::<Vec<_>>(),
        ["C", "C"]
    );
}

#[test]
fn ingests_first_metadata_rows_without_importing_later_rows_or_unselected_files() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("isolated/records.sqlite");
    dataset(
        temp.path(),
        "sample_id\tfilename\nA\ta.mzXML\nB\twrong.txt\nC\tb.mzML\n",
    );
    fs::write(temp.path().join("msdata/raw/unselected.mzML"), "spectrum").unwrap();

    let output = ingest_first(temp.path(), &db, 1);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 sample rows and 1 spectra"));
    let rows = stored(&db);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].sample_id, "A");
    assert_eq!(rows[0].filename, "a.mzXML");
    assert_eq!(rows[0].relative_path, "msdata/raw/a.mzXML");
    let mut conn = SqliteConnection::establish(db.to_str().unwrap()).unwrap();
    let count: Count = diesel::sql_query("SELECT COUNT(*) AS count FROM sample_rows")
        .get_result(&mut conn)
        .unwrap();
    assert_eq!(count.count, 1);

    let failed = ingest_first(temp.path(), &db, 2);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("invalid spectrum filename"));
    assert_eq!(stored(&db).len(), 1);
}

#[test]
fn first_row_with_missing_spectrum_fails_before_creating_database() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("isolated/records.sqlite");
    dataset(temp.path(), "sample_id\tfilename\nA\tmissing.mzML\nB\tb.mzML\n");
    let failed = ingest_first(temp.path(), &db, 1);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("missing file"));
    assert!(!db.exists());
}
