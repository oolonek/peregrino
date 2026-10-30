use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::Connection;

fn fixture(root: &Path, metadata: &str, filenames: &[&str]) -> PathBuf {
    fs::create_dir_all(root.join("metadata")).unwrap();
    fs::create_dir_all(root.join("msdata/raw")).unwrap();
    fs::write(root.join("metadata/metadata.tsv"), metadata).unwrap();
    for filename in filenames {
        fs::write(root.join("msdata/raw").join(filename), "spectrum").unwrap();
    }
    root.join("flock.sqlite")
}

fn try_ingest(root: &Path, db: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_peregrino"))
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
        .output()
        .unwrap()
}

fn ingest(root: &Path, db: &Path) {
    let result = try_ingest(root, db);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn taxa(conn: &Connection) -> Vec<(i64, String)> {
    let mut query = conn
        .prepare("SELECT id, name FROM taxa ORDER BY name")
        .unwrap();
    query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn links(conn: &Connection) -> Vec<(Option<String>, String, Option<String>)> {
    let mut query = conn.prepare("SELECT s.sample_id, json_extract(s.attributes_json, '$.sample_type'), t.name FROM sample_rows s LEFT JOIN taxa t ON t.id = s.taxon_id ORDER BY s.row_number").unwrap();
    query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn massive_scientific_names_link_taxa_without_losing_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let db = fixture(
        root,
        "filename\tfile_path\tdirectus_filename\ttaxon_scientific_name\tionization_mode\npositive.mzML\tdbgi_msdata/positive.mzML\tpositive\t Sapayoa aenigma \tpositive\nnegative.mzML\tdbgi_msdata/negative.mzML\tnegative\tSapayoa aenigma\tnegative\nblank.mzML\tdbgi_msdata/blank.mzML\tblank\t  \tpositive\n",
        &["positive.mzML", "negative.mzML", "blank.mzML"],
    );
    ingest(root, &db);
    let conn = Connection::open(&db).unwrap();
    let names = taxa(&conn);
    assert_eq!(names.len(), 1);
    assert_eq!(names[0].1, "Sapayoa aenigma");
    let mut query = conn.prepare(
        "SELECT s.taxon_id, s.attributes_json, f.filename FROM sample_rows s JOIN spectrum_files f ON f.dataset_id = s.dataset_id AND f.row_number = s.row_number ORDER BY s.row_number"
    ).unwrap();
    let rows: Vec<(Option<i64>, serde_json::Value, String)> = query
        .query_map([], |row| {
            let attributes: String = row.get(1)?;
            Ok((row.get(0)?, serde_json::from_str(&attributes).unwrap(), row.get(2)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].0, Some(names[0].0));
    assert_eq!(rows[1].0, Some(names[0].0));
    assert_eq!(rows[2].0, None);
    for (row, filename, stem, taxon, mode) in [
        (&rows[0], "positive.mzML", "positive", " Sapayoa aenigma ", "positive"),
        (&rows[1], "negative.mzML", "negative", "Sapayoa aenigma", "negative"),
        (&rows[2], "blank.mzML", "blank", "  ", "positive"),
    ] {
        assert_eq!(row.2, filename);
        assert_eq!(row.1, serde_json::json!({
            "filename": filename,
            "file_path": format!("dbgi_msdata/{filename}"),
            "directus_filename": stem,
            "taxon_scientific_name": taxon,
            "ionization_mode": mode,
        }));
    }
}

#[test]
fn biological_samples_share_taxon_while_qc_and_blanks_have_no_taxon() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let db = fixture(
        root,
        "sample_id\tsample_type\tfilename\tsource_taxon\nA\tsample\ta.mzML\tDatura stramonium\nB\tsample\tb.mzML\t Datura stramonium \n\tqc\tc.mzML\t\nBLANK\tblank\td.mzML\t  \n",
        &["a.mzML", "b.mzML", "c.mzML", "d.mzML"],
    );
    ingest(root, &db);
    let conn = Connection::open(&db).unwrap();
    let original = taxa(&conn);
    assert_eq!(original.len(), 1);
    assert_eq!(original[0].1, "Datura stramonium");
    assert_eq!(
        links(&conn),
        vec![
            (
                Some("A".into()),
                "sample".into(),
                Some("Datura stramonium".into())
            ),
            (
                Some("B".into()),
                "sample".into(),
                Some("Datura stramonium".into())
            ),
            (None, "qc".into(), None),
            (Some("BLANK".into()), "blank".into(), None),
        ]
    );

    // The same names retain their IDs across reimports, including when source rows change.
    fs::write(root.join("metadata/metadata.tsv"),
        "sample_id\tsample_type\tfilename\tsource_taxon\nA\tsample\ta.mzML\tSolanum lycopersicum\nB\tsample\tb.mzML\tDatura stramonium\n\tqc\tc.mzML\t\nBLANK\tblank\td.mzML\t\n").unwrap();
    ingest(root, &db);
    assert_eq!(
        taxa(&conn)
            .iter()
            .find(|(_, name)| name == "Datura stramonium")
            .unwrap()
            .0,
        original[0].0
    );
    fs::write(root.join("metadata/metadata.tsv"),
        "sample_id\tsample_type\tfilename\tsource_taxon\nA\tsample\ta.mzML\tSolanum lycopersicum\nB\tsample\tb.mzML\tSolanum lycopersicum\n\tqc\tc.mzML\t\nBLANK\tblank\td.mzML\t\n").unwrap();
    ingest(root, &db);
    assert_eq!(
        taxa(&conn)
            .iter()
            .map(|(_, name)| name.as_str())
            .collect::<Vec<_>>(),
        ["Solanum lycopersicum"]
    );
    fs::write(
        root.join("metadata/metadata.tsv"),
        "sample_id\tsample_type\tfilename\tsource_taxon\nA\tsample\tmissing.mzML\tNew taxon\nB\tsample\tb.mzML\tSolanum lycopersicum\n\tqc\tc.mzML\t\nBLANK\tblank\td.mzML\t\n",
    )
    .unwrap();
    assert!(!try_ingest(root, &db).status.success());
    assert_eq!(
        taxa(&conn)
            .iter()
            .map(|(_, name)| name.as_str())
            .collect::<Vec<_>>(),
        ["Solanum lycopersicum"]
    );
}

#[test]
fn existing_flock_database_migrates_and_accepts_taxon_name_column() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let db = fixture(
        root,
        "sample_id\tsample_type\tfilename\ttaxon_name\nA\tsample\ta.mzML\tDatura stramonium\n\tqc\tb.mzML\t\n",
        &["a.mzML", "b.mzML"],
    );
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch("CREATE TABLE sample_rows (dataset_id INTEGER NOT NULL REFERENCES datasets(id) ON DELETE CASCADE, row_number INTEGER NOT NULL, sample_id TEXT, attributes_json TEXT NOT NULL CHECK (json_valid(attributes_json)), PRIMARY KEY (dataset_id, row_number));").unwrap();
    drop(conn);
    ingest(root, &db);
    let conn = Connection::open(&db).unwrap();
    let foreign_key: String = conn.query_row("SELECT \"table\" FROM pragma_foreign_key_list('sample_rows') WHERE \"from\" = 'taxon_id'", [], |row| row.get(0)).unwrap();
    assert_eq!(foreign_key, "taxa");
    assert_eq!(
        taxa(&conn)
            .iter()
            .map(|(_, name)| name.as_str())
            .collect::<Vec<_>>(),
        ["Datura stramonium"]
    );
    assert_eq!(links(&conn)[1], (None, "qc".into(), None));
    ingest(root, &db);
    assert_eq!(taxa(&conn).len(), 1);
}
