use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use rusqlite::Connection;

fn fixture(root: &Path, metadata: &str, filenames: &[&str], taxon_column: &str) -> PathBuf {
    fs::create_dir_all(root.join("metadata")).unwrap();
    fs::create_dir_all(root.join("msdata/raw")).unwrap();
    fs::write(root.join("metadata/metadata.tsv"), metadata).unwrap();
    fs::write(root.join("taxon.toml"), format!("[dataset]\ntaxon_column = {taxon_column:?}\n")).unwrap();
    for filename in filenames {
        fs::write(root.join("msdata/raw").join(filename), "spectrum").unwrap();
    }
    root.join("flock.sqlite")
}

fn try_ingest(root: &Path, db: &Path) -> Output {
    ingest_with_config(root, db, &root.join("taxon.toml"), None)
}

fn ingest_with_config(root: &Path, db: &Path, config: &Path, selection: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_peregrino"));
    command
        .arg("ingest")
        .arg(root)
        .args(["--doi", "10.5281/zenodo.10018264"])
        .args(["--metadata", "metadata/metadata.tsv", "--ms-dir", "msdata/raw"])
        .arg("--db")
        .arg(db)
        .arg("--config")
        .arg(config);
    if let Some(selection) = selection {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(selection.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    } else {
        command.output().unwrap()
    }
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
        "taxon_scientific_name",
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
        "source_taxon",
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
        "taxon_name",
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

#[test]
fn prompts_for_every_header_and_reuses_exact_saved_nonpreferred_choice() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("dataset");
    let db = fixture(
        &root,
        "sample_id\tfilename\tsource_taxon\tscientific_name\tOther Species\nA\ta.mzML\tWrong first\tWrong second\tChosen species\n",
        &["a.mzML"],
        "source_taxon",
    );
    fs::remove_file(root.join("taxon.toml")).unwrap();
    let config = temp.path().join("settings/peregrino.toml");
    let result = ingest_with_config(&root, &db, &config, Some("5\n"));
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let prompt = String::from_utf8(result.stderr).unwrap();
    for (number, header) in ["sample_id", "filename", "source_taxon", "scientific_name", "Other Species"].iter().enumerate() {
        assert!(prompt.contains(&format!("{}. {}", number + 1, header)), "{prompt}");
    }
    assert_eq!(fs::read_to_string(&config).unwrap(), "[dataset]\ntaxon_column = \"Other Species\"\n");
    let conn = Connection::open(&db).unwrap();
    assert_eq!(taxa(&conn)[0].1, "Chosen species");

    fs::write(root.join("metadata/metadata.tsv"), "sample_id\tfilename\tsource_taxon\tscientific_name\tOther Species\nA\ta.mzML\tStill wrong\tAlso wrong\tSaved species\n").unwrap();
    let rerun = ingest_with_config(&root, &db, &config, None);
    assert!(rerun.status.success(), "{}", String::from_utf8_lossy(&rerun.stderr));
    assert_eq!(taxa(&conn).iter().map(|(_, name)| name.as_str()).collect::<Vec<_>>(), ["Saved species"]);
    assert!(!root.join("taxon.toml").exists());
}

#[test]
fn missing_or_nonexact_configured_column_never_mutates_database() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("dataset");
    let db = fixture(
        &root,
        "sample_id\tfilename\tOther Species\nA\ta.mzML\tChosen species\n",
        &["a.mzML"],
        "Other Species",
    );
    ingest(&root, &db);
    let conn = Connection::open(&db).unwrap();
    let before = taxa(&conn);
    let config = temp.path().join("mato.toml");
    for contents in [
        "[dataset]\ntaxon_column = \"other species\"\n",
        "[dataset]\ntaxon_column = \"absent\"\n",
        "[dataset]\ntaxon_column = \"\"\n",
        "[dataset]\nname = \"dataset\"\n",
        "[dataset]\ntaxon_column = 42\n",
    ] {
        fs::write(&config, contents).unwrap();
        let failed = ingest_with_config(&root, &db, &config, None);
        assert!(!failed.status.success(), "{contents}");
        let error = String::from_utf8_lossy(&failed.stderr);
        assert!(error.contains("taxon column") || error.contains("taxon_column"), "{error}");
        assert_eq!(taxa(&conn), before);
    }
    fs::write(&config, "[dataset]\ntaxon_column = \"Other Species\"\n[repository]\nurl = \"unrelated\"\n").unwrap();
    assert!(ingest_with_config(&root, &db, &config, None).status.success());
    assert_eq!(taxa(&conn), before);
}

#[test]
fn invalid_or_eof_selection_does_not_create_config_or_database() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("dataset");
    let db = fixture(&root, "sample_id\tfilename\tOther Species\nA\ta.mzML\tChosen species\n", &["a.mzML"], "Other Species");
    fs::remove_file(root.join("taxon.toml")).unwrap();
    let config = temp.path().join("settings/peregrino.toml");
    for selection in [Some(""), Some("\n"), Some("0\n"), Some("1,2\n"), Some("4\n"), None] {
        let failed = ingest_with_config(&root, &db, &config, selection);
        assert!(!failed.status.success(), "{selection:?}");
        let error = String::from_utf8_lossy(&failed.stderr);
        assert!(error.contains("no taxon column selected") || error.contains("invalid taxon column selection"), "{error}");
        assert!(!config.exists());
        assert!(!db.exists());
    }
}

#[test]
fn ingest_requires_a_config_argument_before_creating_database() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("dataset");
    let db = fixture(
        &root,
        "sample_id\tfilename\tsource_taxon\nA\ta.mzML\tChosen species\n",
        &["a.mzML"],
        "source_taxon",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_peregrino"))
        .arg("ingest")
        .arg(&root)
        .args(["--doi", "10.5281/zenodo.10018264", "--metadata", "metadata/metadata.tsv", "--ms-dir", "msdata/raw"])
        .arg("--db")
        .arg(&db)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--config"));
    assert!(!db.exists());
}
