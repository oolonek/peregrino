use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_peregrino"))
        .args(args)
        .output()
        .unwrap()
}

fn fixture(root: &Path) -> (PathBuf, PathBuf) {
    let data = root.join("dataset");
    let output = root.join("picoto");
    fs::create_dir_all(data.join("metadata")).unwrap();
    fs::create_dir_all(data.join("msdata/raw")).unwrap();
    fs::create_dir_all(&output).unwrap();
    fs::write(data.join("msdata/raw/run.mzML"), "raw").unwrap();
    fs::write(data.join("metadata/metadata.tsv"), "sample_id\tpositive_filename\tsource_taxon\nA\trun.mzML\tSalvia officinalis\n").unwrap();
    let db = root.join("flock.sqlite");
    let result = cli(&["ingest", data.to_str().unwrap(), "--doi", "10.1234/example", "--metadata", "metadata/metadata.tsv", "--ms-dir", "msdata/raw", "--db", db.to_str().unwrap()]);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    fs::write(output.join("run_features.tsv"), "feature_id\tmz\trt_seconds\tapex_intensity\tapex_to_baseline\tms1_scans\tarea\tisotope_peaks\tquality_score\tstatus\treason\tms2_scan_ids\tms2_quality\tcollision_energy\n1\t123.45\t15\t100000\t3\t3\t100\t0\t5\taccepted\t\t1\t1\t\n\t456.78\t16\t100000\t3\t3\t100\t0\t5\trejected\tno ms2\t\t\t\n").unwrap();
    fs::write(output.join("run_ms2.mgf"), "BEGIN IONS\nFEATURE_ID=1\nTITLE=run.mzML:feature_1:MS2\nPEPMASS=123.45\nCHARGE=1+\nRTINSECONDS=15\nMSLEVEL=2\nFILENAME=run.mzML\n100.1 20\nEND IONS\n").unwrap();
    fs::write(output.join("run_sirius.mgf"), "BEGIN IONS\nFEATURE_ID=1\nTITLE=run.mzML:feature_1:MS1\nPEPMASS=123.45\nCHARGE=1+\nRTINSECONDS=15\nMSLEVEL=1\nFILENAME=run.mzML\n100.1 20\nEND IONS\n\nBEGIN IONS\nFEATURE_ID=1\nTITLE=run.mzML:feature_1:MS2\nPEPMASS=123.45\nCHARGE=1+\nRTINSECONDS=15\nMSLEVEL=2\nFILENAME=run.mzML\n100.1 20\nEND IONS\n").unwrap();
    (data, db)
}

fn register(data: &Path, db: &Path, root: &Path, paired: bool) -> Output {
    let output = root.join("picoto");
    let mut command = Command::new(env!("CARGO_BIN_EXE_peregrino"));
    command.args(["register", "--db"]).arg(db).arg("--dataset").arg(data).args(["--raw", "run.mzML", "--ms2"]).arg(output.join("run_ms2.mgf")).arg("--features").arg(output.join("run_features.tsv"));
    if paired { command.arg("--paired").arg(output.join("run_sirius.mgf")); }
    command.output().unwrap()
}

fn show(db: &Path) -> String {
    let output = cli(&["show", "processed_spectra", "--db", db.to_str().unwrap()]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn registration_migrates_existing_database_without_touching_taxon_resolutions() {
    let root = tempfile::tempdir().unwrap();
    let (data, db) = fixture(root.path());
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("DROP TABLE processed_spectra; CREATE TABLE taxon_resolutions (taxon_id INTEGER PRIMARY KEY REFERENCES taxa(id) ON DELETE CASCADE, qid TEXT, catalogue_of_life_id TEXT, error TEXT); INSERT INTO taxon_resolutions (taxon_id, qid) SELECT id, 'Q100' FROM taxa WHERE name = 'Salvia officinalis';").unwrap();
    drop(conn);
    let result = register(&data, &db, root.path(), false);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(show(&db).lines().count(), 2);
    let ingest = cli(&["ingest", data.to_str().unwrap(), "--doi", "10.1234/example", "--metadata", "metadata/metadata.tsv", "--ms-dir", "msdata/raw", "--db", db.to_str().unwrap()]);
    assert!(ingest.status.success(), "{}", String::from_utf8_lossy(&ingest.stderr));
    let resolutions = cli(&["show", "taxon_resolutions", "--db", db.to_str().unwrap()]);
    assert!(resolutions.status.success());
    assert!(String::from_utf8_lossy(&resolutions.stdout).contains("Q100"));
    assert_eq!(show(&db).lines().count(), 2);
}

#[test]
fn registers_picoto_files_to_existing_raw_and_preserves_same_identity_on_reingest() {
    let root = tempfile::tempdir().unwrap();
    let (data, db) = fixture(root.path());
    let output = register(&data, &db, root.path(), true);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let before = show(&db);
    assert!(before.contains("run_ms2.mgf"), "{before}");
    assert!(before.contains("run_sirius.mgf"), "{before}");
    assert!(before.contains("run_features.tsv"), "{before}");
    assert!(before.contains("e64e1165d6ccaefaf591af5b843ac0b83c444814be1da2f9d56387c34513b78b"), "{before}");
    let result = cli(&["ingest", data.to_str().unwrap(), "--doi", "10.1234/example", "--metadata", "metadata/metadata.tsv", "--ms-dir", "msdata/raw", "--db", db.to_str().unwrap()]);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(show(&db), before);
}

#[test]
fn reingest_prunes_links_when_raw_removed_or_replaced_in_place() {
    let root = tempfile::tempdir().unwrap();
    let (data, db) = fixture(root.path());
    assert!(register(&data, &db, root.path(), false).status.success());
    let raw = data.join("msdata/raw/run.mzML");
    fs::remove_file(&raw).unwrap();
    fs::write(&raw, "different raw").unwrap();
    let ingest = || cli(&["ingest", data.to_str().unwrap(), "--doi", "10.1234/example", "--metadata", "metadata/metadata.tsv", "--ms-dir", "msdata/raw", "--db", db.to_str().unwrap()]);
    assert!(ingest().status.success());
    assert_eq!(show(&db).lines().count(), 1);
    assert!(register(&data, &db, root.path(), false).status.success());
    fs::remove_file(raw).unwrap();
    fs::write(data.join("msdata/raw/remaining.mzML"), "other raw").unwrap();
    fs::write(data.join("metadata/metadata.tsv"), "sample_id\tpositive_filename\tsource_taxon\nB\tremaining.mzML\tSalvia officinalis\n").unwrap();
    assert!(ingest().status.success());
    assert_eq!(show(&db).lines().count(), 1);
}

#[test]
fn registration_rejects_missing_database_mismatched_or_nonpicoto_outputs_without_changes() {
    let root = tempfile::tempdir().unwrap();
    let (data, db) = fixture(root.path());
    let missing = root.path().join("missing.sqlite");
    assert!(!register(&data, &missing, root.path(), false).status.success());
    assert!(!missing.exists());
    assert!(register(&data, &db, root.path(), false).status.success());
    let before = show(&db);
    let mgf = root.path().join("picoto/run_ms2.mgf");
    let valid = fs::read(&mgf).unwrap();
    fs::write(&mgf, "BEGIN IONS\nTITLE=MZmine exported spectrum\nPEPMASS=123.45\n100 10\nEND IONS\n").unwrap();
    assert!(!register(&data, &db, root.path(), false).status.success());
    assert_eq!(show(&db), before);
    fs::write(&mgf, valid).unwrap();
    fs::write(root.path().join("picoto/run_features.tsv"), "feature_id\tmz\trt_seconds\tapex_intensity\tapex_to_baseline\tms1_scans\tarea\tisotope_peaks\tquality_score\tstatus\treason\tms2_scan_ids\tms2_quality\tcollision_energy\n999\t123.45\t15\t100000\t3\t3\t100\t0\t5\taccepted\t\t1\t1\t\n").unwrap();
    assert!(!register(&data, &db, root.path(), false).status.success());
    assert_eq!(show(&db), before);
    let outside = root.path().join("other_ms2.mgf");
    fs::copy(mgf, outside).unwrap();
    let result = cli(&["register", "--db", db.to_str().unwrap(), "--dataset", data.to_str().unwrap(), "--raw", "run.mzML", "--ms2", root.path().join("other_ms2.mgf").to_str().unwrap(), "--features", root.path().join("picoto/run_features.tsv").to_str().unwrap()]);
    assert!(!result.status.success());
}
