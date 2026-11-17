use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use rusqlite::{Connection, params};

const HEADER: &str = "source_value\twikidata_qid\treviewer\trationale\n";
const LEAF: &str = "Leaf\tQ33971\tA. Reviewer\tReviewed against Wikidata leaf item\n";

fn import(db: &Path, file: &Path, dataset_id: &str, field: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_peregrino"))
        .args(["kg", "map", "import", "--db"])
        .arg(db)
        .args(["--dataset-id", dataset_id, "--field", field, "--file"])
        .arg(file)
        .output()
        .unwrap()
}

fn legacy_db(path: &Path) {
    let db = Connection::open(path).unwrap();
    db.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE datasets (id INTEGER PRIMARY KEY, doi TEXT NOT NULL);
         INSERT INTO datasets (id, doi) VALUES (1, '10.1234/example');
         CREATE TABLE sample_rows (
           dataset_id INTEGER NOT NULL REFERENCES datasets(id),
           row_number INTEGER NOT NULL,
           attributes_json TEXT NOT NULL,
           PRIMARY KEY (dataset_id, row_number)
         );
         INSERT INTO sample_rows VALUES (1, 1, '{\"observation_subject\":\"Leaf\"}');
         INSERT INTO sample_rows VALUES (1, 2, '{\"observation_subject\":\"\"}');",
    )
    .unwrap();
}

fn versions(db: &Connection) -> Vec<(i64, i64, String, String, String, String)> {
    db.prepare(
        "SELECT i.id, i.active, i.file_sha256, d.wikidata_qid, d.reviewer, d.rationale
         FROM kg_mapping_imports i JOIN kg_mapping_decisions d ON d.import_id = i.id
         ORDER BY i.id",
    )
    .unwrap()
    .query_map([], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

#[test]
fn importing_reviewed_subjects_backfills_legacy_rows_and_preserves_immutable_versions() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("legacy.sqlite");
    let file = temp.path().join("review.tsv");
    legacy_db(&db_path);
    fs::write(&file, format!("{HEADER}{LEAF}")).unwrap();

    let first = import(&db_path, &file, "1", "observation_subject");
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    assert!(String::from_utf8_lossy(&first.stdout).contains("rows=1"));
    let db = Connection::open(&db_path).unwrap();
    assert_eq!(db.query_row("SELECT observation_subject FROM sample_rows WHERE row_number = 1", [], |row| row.get::<_, String>(0)).unwrap(), "Leaf");
    assert_eq!(db.query_row("SELECT observation_subject FROM sample_rows WHERE row_number = 2", [], |row| row.get::<_, Option<String>>(0)).unwrap(), None);
    let first_version = versions(&db);
    assert_eq!(first_version.len(), 1);
    assert_eq!(first_version[0].1, 1);
    assert_eq!(first_version[0].3, "Q33971");
    assert_eq!(first_version[0].4, "A. Reviewer");
    assert_eq!(first_version[0].5, "Reviewed against Wikidata leaf item");

    let repeated = import(&db_path, &file, "1", "observation_subject");
    assert!(repeated.status.success(), "{}", String::from_utf8_lossy(&repeated.stderr));
    assert!(String::from_utf8_lossy(&repeated.stdout).contains("unchanged"));
    assert_eq!(versions(&db), first_version);

    fs::write(&file, format!("{HEADER}Leaf\tQ33971\tB. Reviewer\tSecond independent review\n")).unwrap();
    let changed = import(&db_path, &file, "1", "observation_subject");
    assert!(changed.status.success(), "{}", String::from_utf8_lossy(&changed.stderr));
    let second_version = versions(&db);
    assert_eq!(second_version.len(), 2);
    assert_eq!(second_version[0].1, 0);
    assert_eq!(second_version[0].3, "Q33971");
    assert_eq!(second_version[0].4, "A. Reviewer");
    assert_eq!(second_version[1].1, 1);
    assert_eq!(second_version[1].4, "B. Reviewer");
    assert_ne!(second_version[0].2, second_version[1].2);

    fs::write(&file, format!("{HEADER}{LEAF}")).unwrap();
    let reactivated = import(&db_path, &file, "1", "observation_subject");
    assert!(reactivated.status.success(), "{}", String::from_utf8_lossy(&reactivated.stderr));
    let third_version = versions(&db);
    assert_eq!(third_version.len(), 3);
    assert_eq!(third_version[0].1, 0);
    assert_eq!(third_version[1].1, 0);
    assert_eq!(third_version[2].1, 1);
    assert_eq!(third_version[0].2, third_version[2].2);
    assert_eq!(third_version[0].4, third_version[2].4);
}

#[test]
fn invalid_import_does_not_replace_the_active_decision() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("legacy.sqlite");
    let file = temp.path().join("review.tsv");
    legacy_db(&db_path);
    fs::write(&file, format!("{HEADER}{LEAF}")).unwrap();
    assert!(import(&db_path, &file, "1", "observation_subject").status.success());
    let db = Connection::open(&db_path).unwrap();
    let before = versions(&db);
    for (invalid, field, dataset_id) in [
        (format!("{HEADER}leaf\tQ33971\tA\tcase mismatch\n"), "observation_subject", "1"),
        (format!("{HEADER}Leaf\tQ0\tA\tinvalid ID\n"), "observation_subject", "1"),
        (format!("{HEADER}{LEAF}{LEAF}"), "observation_subject", "1"),
        (format!("{HEADER}\tQ33971\tA\tblank subject\n"), "observation_subject", "1"),
        (format!("{HEADER}Leaf\tQ33971\t \treviewer missing\n"), "observation_subject", "1"),
        (format!("{HEADER}{LEAF}"), "wrong_field", "1"),
        (format!("{HEADER}{LEAF}"), "observation_subject", "999"),
    ] {
        fs::write(&file, invalid).unwrap();
        let result = import(&db_path, &file, dataset_id, field);
        assert!(!result.status.success(), "unexpectedly accepted {field}: {}", String::from_utf8_lossy(&result.stdout));
        assert_eq!(versions(&db), before);
    }
    let nonexistent = temp.path().join("missing.sqlite");
    fs::write(&file, format!("{HEADER}{LEAF}")).unwrap();
    assert!(!import(&nonexistent, &file, "1", "observation_subject").status.success());
    assert!(!nonexistent.exists());
    assert_eq!(db.query_row("SELECT COUNT(*) FROM kg_mapping_decisions WHERE import_id=?1", params![before[0].0], |row| row.get::<_, i64>(0)).unwrap(), 1);
}

#[test]
fn a_new_empty_review_withdraws_leaf_links_without_erasing_older_approval() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("legacy.sqlite");
    let file = temp.path().join("review.tsv");
    legacy_db(&db_path);
    fs::write(&file, format!("{HEADER}{LEAF}")).unwrap();
    assert!(import(&db_path, &file, "1", "observation_subject").status.success());
    fs::write(&file, HEADER).unwrap();
    let withdrawn = import(&db_path, &file, "1", "observation_subject");
    assert!(
        withdrawn.status.success(),
        "{}",
        String::from_utf8_lossy(&withdrawn.stderr)
    );
    let db = Connection::open(&db_path).unwrap();
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM kg_mapping_decisions d JOIN kg_mapping_imports i ON d.import_id=i.id WHERE i.active=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    let history = versions(&db);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].1, 0);
}
