use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_peregrino"))
        .args(args)
        .output()
        .unwrap()
}

struct Fixture {
    root: tempfile::TempDir,
    db: PathBuf,
    mgf: PathBuf,
    graph: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("flock.sqlite");
        let graph = root.path().join("graph.nq");
        let metadata = root.path().join("metadata.tsv");
        let raw = root.path().join("run.mzML");
        let audit = root.path().join("run_features.tsv");
        let mgf = root.path().join("run_ms2.mgf");
        let config = root.path().join("matcher.toml");
        fs::write(
            &metadata,
            "sample_id\tsource_taxon\nA\tSalvia officinalis\n",
        )
        .unwrap();
        fs::write(&raw, b"raw spectral bytes").unwrap();
        fs::write(&audit, "feature_id\tmz\trt_seconds\tapex_intensity\tapex_to_baseline\tms1_scans\tarea\tisotope_peaks\tquality_score\tstatus\treason\tms2_scan_ids\tms2_quality\tcollision_energy\n1\t123.45\t15.5\t100\t3\t3\t100\t0\t5\taccepted\t\t1\t1\t\n2\t223.45\t32\t100\t3\t3\t100\t0\t5\taccepted\t\t2\t1\t\n").unwrap();
        let mgf_bytes = b"BEGIN IONS\nFEATURE_ID=1\nTITLE=run.mzML:feature_1:MS2\nMSLEVEL=2\nFILENAME=run.mzML\nPEPMASS=123.45000000\nCHARGE=1+\nRTINSECONDS=15.5000\n100 20\nEND IONS\nBEGIN IONS\nFEATURE_ID=2\nTITLE=run.mzML:feature_2:MS2\nMSLEVEL=2\nFILENAME=run.mzML\nPEPMASS=223.45000000\nCHARGE=1+\nRTINSECONDS=32.0000\n200 20\nEND IONS\n";
        fs::write(&mgf, mgf_bytes).unwrap();
        fs::write(&config, "[search]\nmetric = 'ModifiedLinearCosine'\n").unwrap();
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE datasets (id INTEGER PRIMARY KEY,doi TEXT,root TEXT,metadata_path TEXT);
            CREATE TABLE taxa(id INTEGER PRIMARY KEY,name TEXT);
            CREATE TABLE taxon_resolutions(taxon_id INTEGER,qid TEXT);
            CREATE TABLE sample_rows(dataset_id INTEGER,row_number INTEGER,sample_id TEXT,taxon_id INTEGER,attributes_json TEXT);
            CREATE TABLE spectrum_files(dataset_id INTEGER,filename TEXT,row_number INTEGER,relative_path TEXT);
            CREATE TABLE processed_spectra(dataset_id INTEGER,filename TEXT,ms2_path TEXT,ms2_sha256 TEXT,features_path TEXT,paired_path TEXT);
            CREATE TABLE annotation_runs(id INTEGER PRIMARY KEY,dataset_id INTEGER,filename TEXT,ms2_sha256 TEXT,registered_ms2_sha256 TEXT,library_sha256 TEXT,lotus_csv_sha256 TEXT,config_sha256 TEXT,config_path TEXT,status TEXT,error TEXT,taxon_name TEXT,taxon_qid TEXT,taxonomy_query TEXT,matcher_version TEXT,created_at TEXT);
            CREATE TABLE annotation_features(run_id INTEGER,feature_id TEXT,precursor_mz REAL,status TEXT);
            CREATE TABLE annotation_hits(run_id INTEGER,feature_id TEXT,rank INTEGER,library_spectrum_id TEXT,library_inchikey TEXT,spectral_score REAL,taxonomic_score REAL,combined_score REAL,rank_before_taxonomy INTEGER,taxonomic_support TEXT,matches INTEGER,ms1_deviation_ppm REAL,matched_organism_name TEXT,matched_organism_wikidata TEXT,matched_shared_rank TEXT,matched_lotus_inchikey TEXT,matched_lotus_structure_wikidata TEXT,matched_lotus_reference_wikidata TEXT,matched_lotus_reference_doi TEXT);
            CREATE TABLE chemical_lookups(inchikey TEXT,status TEXT);
            CREATE TABLE chemical_entities(inchikey TEXT,wikidata_qid TEXT);
            INSERT INTO taxa VALUES(1,'Salvia officinalis');
            INSERT INTO taxon_resolutions VALUES(1,'Q111');
            INSERT INTO sample_rows VALUES(1,1,'A',1,'{\"sample_id\":\"A\"}');
            INSERT INTO spectrum_files VALUES(1,'run.mzML',1,'run.mzML');
            INSERT INTO annotation_features VALUES(1,'1',123.45,'hits'),(1,'2',223.45,'no_hits');
            INSERT INTO annotation_hits VALUES(1,'1',1,'reference-1','AAAAAAAAAAAAAA-BBBBBBBBBB-C',0.8,9,9.8,2,'supported',8,1.25,'Salvia officinalis','http://www.wikidata.org/entity/Q999','species','AAAAAAAAAAAAAA-CCCCCCCCCC-D','http://www.wikidata.org/entity/Q333','http://www.wikidata.org/entity/Q444','10.1234/support'),
                (1,'1',2,'reference-2','ZZZZZZZZZZZZZZ-YYYYYYYYYY-X',0.7,0,0.7,2,'unsupported',3,2.5,NULL,NULL,NULL,NULL,NULL,NULL,NULL);
            INSERT INTO chemical_lookups VALUES('AAAAAAAAAAAAAA-BBBBBBBBBB-C','matched'),('ZZZZZZZZZZZZZZ-YYYYYYYYYY-X','ambiguous');
            INSERT INTO chemical_entities VALUES('AAAAAAAAAAAAAA-BBBBBBBBBB-C','Q222'),('ZZZZZZZZZZZZZZ-YYYYYYYYYY-X','Q333'),('ZZZZZZZZZZZZZZ-YYYYYYYYYY-X','Q444');").unwrap();
        conn.execute(
            "INSERT INTO datasets VALUES (1,?1,?2,?3)",
            params![
                "10.1234/graph",
                root.path().to_str().unwrap(),
                metadata.to_str().unwrap()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO processed_spectra VALUES (1,'run.mzML',?1,?2,?3,NULL)",
            params![
                mgf.to_str().unwrap(),
                sha(mgf_bytes),
                audit.to_str().unwrap()
            ],
        )
        .unwrap();
        conn.execute("INSERT INTO annotation_runs VALUES (1,1,'run.mzML',?1,?1,'library-digest','lotus-digest',?2,?3,'hits',NULL,'Salvia officinalis','Q111','Salvia officinalis','spectral-matcher/0.1.0','2025-01-01T00:00:00Z')", params![sha(mgf_bytes), sha(&fs::read(&config).unwrap()), config.to_str().unwrap()]).unwrap();
        Self {
            root,
            db,
            mgf,
            graph,
        }
    }
    fn export(&self) -> Output {
        cli(&[
            "kg",
            "export",
            "--db",
            self.db.to_str().unwrap(),
            "--dataset-id",
            "1",
            "--out",
            self.graph.to_str().unwrap(),
        ])
    }
    fn select(&self, sparql: &str) -> String {
        let query = self.root.path().join("query.rq");
        fs::write(&query, sparql).unwrap();
        let result = cli(&[
            "kg",
            "query",
            "--graph",
            self.graph.to_str().unwrap(),
            "--query",
            query.to_str().unwrap(),
        ]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap()
    }
}

#[test]
fn export_and_query_preserve_provenance_and_candidate_not_presence() {
    let fixture = Fixture::new();
    let export = fixture.export();
    assert!(
        export.status.success(),
        "{}",
        String::from_utf8_lossy(&export.stderr)
    );
    let query = "PREFIX k: <urn:mato:term:> PREFIX prov: <http://www.w3.org/ns/prov#>\nSELECT ?taxon ?chemical ?rawhash ?mz ?rt ?score WHERE { ?dataset k:hasSampleRecord ?sample . ?sample k:reportedSourceTaxon ?taxon ; k:hasAcquisition ?acquisition . ?acquisition prov:used ?sample ; k:rawFile ?raw . ?raw k:sha256 ?rawhash . ?processing prov:used ?raw ; k:hasFeature ?feature . ?feature k:precursorMz ?mz ; k:retentionTimeSeconds ?rt ; k:querySpectrum ?spectrum . ?run k:hasCandidateHit ?hit . ?hit k:feature ?feature ; k:candidateChemical ?chemical ; k:spectralScore ?score }";
    let result = fixture.select(query);
    assert!(
        result.contains(
            "<http://www.wikidata.org/entity/Q111>\t<http://www.wikidata.org/entity/Q222>"
        ),
        "{result}"
    );
    assert!(result.contains(&sha(b"raw spectral bytes")), "{result}");
    assert!(
        result.contains("123.45") && result.contains("15.5") && result.contains("0.8"),
        "{result}"
    );
    let evidence = fixture.select("PREFIX k: <urn:mato:term:> SELECT ?taxon ?organism ?support ?sharedRank ?peaks ?version WHERE { ?run k:scoringTaxon ?taxon ; k:matcherVersion ?version ; k:hasCandidateHit ?hit . ?hit k:matchedOrganism ?organism ; k:taxonomicSupport ?support ; k:matchedSharedRank ?sharedRank ; k:matchedPeakCount ?peaks }");
    assert!(evidence.contains("<http://www.wikidata.org/entity/Q111>\t<http://www.wikidata.org/entity/Q999>\t\"supported\"\t\"species\"\t8\t\"spectral-matcher/0.1.0\""), "{evidence}");
    let supporting_row = fixture.select("PREFIX k: <urn:mato:term:> SELECT ?candidate ?candidateKey ?source ?sourceKey ?citation ?doi WHERE { ?run k:hasCandidateHit ?hit . ?hit k:candidateChemical ?candidate ; k:inchikey ?candidateKey ; k:matchedLotusStructure ?source ; k:matchedLotusInchikey ?sourceKey ; k:matchedLotusReference ?citation ; k:matchedLotusReferenceDoi ?doi }");
    assert!(supporting_row.contains("<http://www.wikidata.org/entity/Q222>\t\"AAAAAAAAAAAAAA-BBBBBBBBBB-C\"\t<http://www.wikidata.org/entity/Q333>\t\"AAAAAAAAAAAAAA-CCCCCCCCCC-D\"\t<http://www.wikidata.org/entity/Q444>\t\"10.1234/support\""), "{supporting_row}");
    let counts = fixture.select("PREFIX k: <urn:mato:term:> SELECT (COUNT(DISTINCT ?f) AS ?n) WHERE { ?p k:hasFeature ?f . ?f a k:Feature ; k:querySpectrum ?q . }");
    assert_eq!(counts.lines().nth(1), Some("2"), "{counts}");
    let ambiguous = fixture.select("PREFIX k: <urn:mato:term:> SELECT ?chemical WHERE { ?hit k:inchikey \"ZZZZZZZZZZZZZZ-YYYYYYYYYY-X\" ; k:candidateChemical ?chemical }");
    assert_eq!(ambiguous.lines().count(), 1, "{ambiguous}");
    let presence = fixture.select("PREFIX k: <urn:mato:term:> SELECT ?owner ?chemical WHERE { ?owner k:candidateChemical ?chemical . FILTER EXISTS { ?owner a k:SampleRecord } }");
    assert_eq!(presence.lines().count(), 1, "{presence}");
    let before = fs::read(&fixture.graph).unwrap();
    assert!(
        !fixture.export().status.success(),
        "must not overwrite graph"
    );
    assert_eq!(before, fs::read(&fixture.graph).unwrap());
}

#[test]
fn missing_or_modified_registered_mgf_fails_without_publishing_graph() {
    let fixture = Fixture::new();
    fs::write(&fixture.mgf, "modified").unwrap();
    let changed = fixture.export();
    assert!(!changed.status.success());
    assert!(!fixture.graph.exists());
    assert!(String::from_utf8_lossy(&changed.stderr).contains("SHA-256"));
    fs::remove_file(&fixture.mgf).unwrap();
    assert!(!fixture.export().status.success());
    assert!(!fixture.graph.exists());
}

#[test]
fn export_rejects_registered_mgf_without_picoto_title_even_when_hash_matches() {
    let fixture = Fixture::new();
    let original = fs::read_to_string(&fixture.mgf).unwrap();
    let changed = original.replace("TITLE=run.mzML:feature_1:MS2\n", "");
    fs::write(&fixture.mgf, changed.as_bytes()).unwrap();
    let conn = Connection::open(&fixture.db).unwrap();
    conn.execute(
        "UPDATE processed_spectra SET ms2_sha256=?1 WHERE dataset_id=1 AND filename='run.mzML'",
        [sha(changed.as_bytes())],
    ).unwrap();
    let result = fixture.export();
    assert!(!result.status.success());
    assert!(!fixture.graph.exists());
    assert!(String::from_utf8_lossy(&result.stderr).contains("FILENAME/TITLE"));
}

#[test]
fn changed_audit_coordinates_cannot_be_misattributed_to_registered_spectra() {
    let fixture = Fixture::new();
    let audit = fixture.root.path().join("run_features.tsv");
    let original = fs::read_to_string(&audit).unwrap();
    fs::write(&audit, original.replace("123.45\t15.5", "123.45\t25.5")).unwrap();
    let result = fixture.export();
    assert!(!result.status.success());
    assert!(!fixture.graph.exists());
    assert!(String::from_utf8_lossy(&result.stderr).contains("disagrees with accepted audit"));
}

#[test]
fn historical_runs_remain_visible_without_falsely_linking_stale_hits() {
    let fixture = Fixture::new();
    let conn = Connection::open(&fixture.db).unwrap();
    conn.execute(
        "UPDATE annotation_runs SET ms2_sha256='old-ms2' WHERE id=1",
        [],
    )
    .unwrap();
    assert!(fixture.export().status.success());
    let result = fixture.select("PREFIX k: <urn:mato:term:> SELECT ?run ?hit ?feature WHERE { ?run a k:AnnotationRun ; k:hasCandidateHit ?hit . OPTIONAL { ?hit k:feature ?feature } } ORDER BY ?hit");
    assert_eq!(result.lines().count(), 3, "{result}");
    for line in result.lines().skip(1) {
        assert!(line.ends_with('\t'), "{line}");
    }
}

#[test]
fn export_without_optional_reconciliation_annotation_or_registration_keeps_acquisition() {
    let fixture = Fixture::new();
    let conn = Connection::open(&fixture.db).unwrap();
    conn.execute_batch(
        "DROP TABLE processed_spectra; DROP TABLE taxon_resolutions;
        DROP TABLE annotation_hits; DROP TABLE annotation_features; DROP TABLE annotation_runs;
        DROP TABLE chemical_entities; DROP TABLE chemical_lookups;",
    )
    .unwrap();
    assert!(fixture.export().status.success());
    let output = fixture.select("PREFIX k: <urn:mato:term:> SELECT ?sample ?raw WHERE { ?sample a k:SampleRecord ; k:hasAcquisition ?acquisition . ?acquisition k:rawFile ?raw . ?raw a k:RawFile }");
    assert_eq!(output.lines().count(), 2, "{output}");
    let absent =
        fixture.select("PREFIX k: <urn:mato:term:> SELECT ?feature WHERE { ?feature a k:Feature }");
    assert_eq!(absent.lines().count(), 1, "{absent}");
}

#[test]
fn export_is_byte_deterministic_for_the_same_snapshot() {
    let fixture = Fixture::new();
    assert!(fixture.export().status.success());
    // The ingested rows, not later edits to their source TSV, determine graph identity.
    fs::write(
        fixture.root.path().join("metadata.tsv"),
        "sample_id\tsource_taxon\nB\tOther\n",
    )
    .unwrap();
    let other = fixture.root.path().join("second.nq");
    let result = cli(&[
        "kg",
        "export",
        "--db",
        fixture.db.to_str().unwrap(),
        "--dataset-id",
        "1",
        "--out",
        other.to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read(&fixture.graph).unwrap(), fs::read(other).unwrap());
}

#[test]
fn native_shacl_validation_rejects_an_export_missing_its_dataset_doi() {
    let fixture = Fixture::new();
    assert!(fixture.export().status.success());
    let graph = fixture.graph.to_str().unwrap();
    let conforming = cli(&["kg", "validate", "--graph", graph]);
    assert!(
        conforming.status.success(),
        "{}",
        String::from_utf8_lossy(&conforming.stderr)
    );
    let original = fs::read_to_string(&fixture.graph).unwrap();
    let invalid = original.replace(
        "<http://www.wikidata.org/prop/direct/P356>",
        "<urn:mato:term:wrongProperty>",
    );
    assert_ne!(invalid, original);
    fs::write(&fixture.graph, invalid).unwrap();
    let nonconforming = cli(&["kg", "validate", "--graph", graph]);
    assert!(!nonconforming.status.success());
    assert!(
        String::from_utf8_lossy(&nonconforming.stderr).contains("SHACL validation failed"),
        "{}",
        String::from_utf8_lossy(&nonconforming.stderr)
    );
}
