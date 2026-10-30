use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use diesel::connection::SimpleConnection;
use diesel::prelude::*;

fn command(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_peregrino"))
        .args(args)
        .output()
        .unwrap()
}

fn populated(root: &Path) -> std::path::PathBuf {
    fs::create_dir_all(root.join("metadata")).unwrap();
    fs::create_dir_all(root.join("msdata/raw")).unwrap();
    fs::write(
        root.join("metadata/metadata.tsv"),
        "sample_id\tfilename\nA\ta.mzXML\nB\tb.mzML\n",
    )
    .unwrap();
    fs::write(root.join("msdata/raw/a.mzXML"), "spectrum").unwrap();
    fs::write(root.join("msdata/raw/b.mzML"), "spectrum").unwrap();
    let db = root.join("flock.sqlite");
    let result = command(&[
        "ingest",
        root.to_str().unwrap(),
        "--doi",
        "10.5281/zenodo.10018264",
        "--metadata",
        "metadata/metadata.tsv",
        "--ms-dir",
        "msdata/raw",
        "--db",
        db.to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    db
}

#[test]
fn lists_user_tables_and_previews_a_bounded_number_of_rows() {
    let temp = tempfile::tempdir().unwrap();
    let db = populated(temp.path());
    let db_path = db.to_str().unwrap();
    let tables = command(&["tables", "--db", db_path]);
    assert!(tables.status.success());
    let tables = String::from_utf8(tables.stdout).unwrap();
    assert!(tables.lines().any(|table| table == "processed_spectra"));

    let preview = command(&["show", "sample_rows", "--db", db_path, "--limit", "1"]);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let mut tsv = csv::ReaderBuilder::new()
        .delimiter(b'\t')
        .from_reader(preview.stdout.as_slice());
    assert_eq!(
        tsv.headers().unwrap().iter().collect::<Vec<_>>(),
        [
            "dataset_id",
            "row_number",
            "sample_id",
            "taxon_id",
            "attributes_json"
        ]
    );
    let rows = tsv.records().map(Result::unwrap).collect::<Vec<_>>();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get(2), Some("A"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(rows[0].get(4).unwrap()).unwrap()["filename"],
        "a.mzXML"
    );
}

#[test]
fn exports_all_spectra_and_does_not_replace_an_existing_file() {
    let temp = tempfile::tempdir().unwrap();
    let db = populated(temp.path());
    let out = temp.path().join("exports/spectra.tsv");
    let args = [
        "export",
        "spectrum_files",
        "--db",
        db.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ];
    let first = command(&args);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let contents = fs::read(&out).unwrap();
    let mut tsv = csv::ReaderBuilder::new()
        .delimiter(b'\t')
        .from_reader(contents.as_slice());
    let rows = tsv.records().map(Result::unwrap).collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].get(1), Some("b.mzML"));
    assert_eq!(rows[1].get(4), Some("msdata/raw/b.mzML"));
    assert!(!command(&args).status.success());
    assert_eq!(fs::read(&out).unwrap(), contents);
}

#[test]
fn rejects_unknown_tables_and_does_not_create_a_missing_database() {
    let temp = tempfile::tempdir().unwrap();
    let db = populated(temp.path());
    let error = command(&[
        "show",
        "sample_rows; DROP TABLE datasets",
        "--db",
        db.to_str().unwrap(),
    ]);
    assert!(!error.status.success());
    assert!(String::from_utf8_lossy(&error.stderr).contains("not found"));
    assert!(
        command(&["tables", "--db", db.to_str().unwrap()])
            .status
            .success()
    );
    let missing = temp.path().join("missing.sqlite");
    assert!(
        !command(&["tables", "--db", missing.to_str().unwrap()])
            .status
            .success()
    );
    assert!(!missing.exists());
}

#[test]
fn quotes_unusual_table_names_and_escapes_tsv_cells() {
    let temp = tempfile::tempdir().unwrap();
    let db = populated(temp.path());
    let mut conn = SqliteConnection::establish(db.to_str().unwrap()).unwrap();
    conn.batch_execute("CREATE TABLE \"odd\"\"table\" (value TEXT, missing TEXT, bytes BLOB); INSERT INTO \"odd\"\"table\" VALUES ('line 1' || char(10) || 'line 2' || char(9) || 'quoted', NULL, X'00FF');").unwrap();
    let preview = command(&["show", "odd\"table", "--db", db.to_str().unwrap()]);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let mut tsv = csv::ReaderBuilder::new()
        .delimiter(b'\t')
        .from_reader(preview.stdout.as_slice());
    let row = tsv.records().next().unwrap().unwrap();
    assert_eq!(row.get(0), Some("line 1\nline 2\tquoted"));
    assert_eq!(row.get(1), Some(""));
    assert_eq!(row.get(2), Some("0x00ff"));
}

#[test]
fn annotation_status_views_can_be_listed_and_previewed() {
    let temp = tempfile::tempdir().unwrap();
    let db = populated(temp.path());
    let mut conn = SqliteConnection::establish(db.to_str().unwrap()).unwrap();
    conn.batch_execute("CREATE VIEW annotation_run_status AS SELECT filename, 0 AS is_current FROM spectrum_files").unwrap();
    let path = db.to_str().unwrap();
    let listed = command(&["tables", "--db", path]);
    assert!(listed.status.success());
    assert!(String::from_utf8_lossy(&listed.stdout).lines().any(|name| name == "annotation_run_status"));
    let preview = command(&["show", "annotation_run_status", "--db", path]);
    assert!(preview.status.success(), "{}", String::from_utf8_lossy(&preview.stderr));
    let result = String::from_utf8(preview.stdout).unwrap();
    assert!(result.contains("a.mzXML\t0"));
}
