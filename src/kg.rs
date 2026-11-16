//! Read-only projection of the committed SQLite dataset and its registered file bytes.
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use indicatif::{ProgressBar, ProgressStyle};
use oxigraph::io::{RdfFormat, RdfParser, RdfSerializer, WriterQuadSerializer};
use oxigraph::model::{Literal, NamedNode, Term, Triple};
use oxigraph::sparql::QueryResults;
use oxigraph::sparql::results::{QueryResultsFormat, QueryResultsSerializer};
use oxigraph::store::Store;
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use crate::artifact;

const TERM: &str = "urn:mato:term:";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const PROV: &str = "http://www.w3.org/ns/prov#";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

fn node(value: &str) -> Result<NamedNode> {
    Ok(NamedNode::new(value)?)
}
fn local(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    format!("urn:mato:local:{:x}", hash.finalize())
}
fn term(name: &str) -> String {
    format!("{TERM}{name}")
}
fn qid(value: &str) -> Option<String> {
    let id = value
        .strip_prefix("http://www.wikidata.org/entity/")
        .or_else(|| value.strip_prefix("https://www.wikidata.org/entity/"))
        .unwrap_or(value);
    let number = id.strip_prefix('Q')?;
    (!number.is_empty()
        && number.bytes().all(|c| c.is_ascii_digit())
        && number != "0"
        && !number.starts_with('0'))
    .then(|| format!("http://www.wikidata.org/entity/{id}"))
}
fn full_inchikey(value: &str) -> bool {
    let mut parts = value.split('-');
    matches!((parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(first), Some(second), Some(third), None)
        if first.len() == 14 && second.len() == 10 && third.len() == 1
            && [first, second, third].iter().all(|part| part.bytes().all(|c| c.is_ascii_uppercase())))
}
fn file_hash(path: &Path) -> Result<String> {
    let mut reader = File::open(path).with_context(|| format!("read {}", path.display()))?;
    let mut hash = Sha256::new();
    let mut chunk = [0u8; 65536];
    loop {
        let n = reader.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        hash.update(&chunk[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn table(db: &Connection, name: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [name],
        |r| r.get::<_, bool>(0),
    )?)
}

// Ingested sample rows, not a mutable metadata file on disk, define this snapshot.
fn sample_rows_hash(db: &Connection, dataset_id: i64) -> Result<String> {
    let mut stmt = db.prepare(
        "SELECT row_number, sample_id, attributes_json FROM sample_rows \
         WHERE dataset_id=?1 ORDER BY row_number",
    )?;
    let mut rows = stmt.query([dataset_id])?;
    let mut hash = Sha256::new();
    while let Some(row) = rows.next()? {
        let number: i64 = row.get(0)?;
        let sample_id: Option<String> = row.get(1)?;
        let attributes: String = row.get(2)?;
        hash.update(number.to_be_bytes());
        hash.update([u8::from(sample_id.is_some())]);
        for value in [sample_id.as_deref().unwrap_or(""), attributes.as_str()] {
            hash.update((value.len() as u64).to_be_bytes());
            hash.update(value.as_bytes());
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}

struct Graph<W: Write>(WriterQuadSerializer<W>);
impl<W: Write> Graph<W> {
    fn add(&mut self, s: &str, p: &str, o: Term) -> Result<()> {
        let triple = Triple::new(node(s)?, node(p)?, o);
        self.0.serialize_triple(&triple)?;
        Ok(())
    }
    fn link(&mut self, s: &str, p: &str, o: &str) -> Result<()> {
        self.add(s, p, node(o)?.into())
    }
    fn property(&mut self, s: &str, p: &str, o: &str) -> Result<()> {
        self.link(s, &term(p), o)
    }
    fn text(&mut self, s: &str, p: &str, value: &str) -> Result<()> {
        self.add(s, &term(p), Literal::new_simple_literal(value).into())
    }
    fn typed(&mut self, s: &str, p: &str, value: &str, datatype: &str) -> Result<()> {
        self.add(
            s,
            &term(p),
            Literal::new_typed_literal(value, node(datatype)?).into(),
        )
    }
    fn class(&mut self, s: &str, class: &str) -> Result<()> {
        self.link(s, RDF_TYPE, &term(class))
    }
    fn hash(&mut self, s: &str, digest: &str) -> Result<()> {
        self.text(s, "sha256", digest)
    }
    fn used(&mut self, s: &str, o: &str) -> Result<()> {
        self.link(s, &format!("{PROV}used"), o)
    }
    fn produced(&mut self, s: &str, activity: &str) -> Result<()> {
        self.link(s, &format!("{PROV}wasGeneratedBy"), activity)
    }
}

#[derive(Clone)]
struct Processed {
    uri: String,
    ms2_hash: String,
    features: BTreeMap<String, (String, String)>,
}

/// Export one dataset as deterministic default-graph N-Quads, without writing to SQLite.
pub fn export(database: &Path, dataset_id: i64, output: &Path) -> Result<()> {
    ensure!(
        database.is_file(),
        "database does not exist: {}",
        database.display()
    );
    ensure!(
        !output.exists(),
        "output already exists: {}",
        output.display()
    );
    let db = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.execute_batch("BEGIN")?; // one repeatable SQLite snapshot, even under concurrent writers
    let (doi, root): (String, String) = db
        .query_row(
            "SELECT doi, root FROM datasets WHERE id=?1",
            [dataset_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .context("dataset ID not found")?;
    let metadata_hash = sample_rows_hash(&db, dataset_id)?;
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut graph = Graph(
            RdfSerializer::from_format(RdfFormat::NQuads)
                .for_writer(BufWriter::new(tmp.as_file_mut())),
        );
        let dataset = local(&["dataset", &doi]);
        graph.class(&dataset, "Dataset")?;
        graph.add(
            &dataset,
            "http://www.wikidata.org/prop/direct/P356",
            Literal::new_simple_literal(doi.as_str()).into(),
        )?;
        let mut samples = BTreeMap::new();
        let mut rows = db.prepare("SELECT s.row_number, s.sample_id, s.attributes_json, s.taxon_id FROM sample_rows s WHERE s.dataset_id=?1 ORDER BY s.row_number")?;
        let rows = rows.query_map([dataset_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })?;
        let has_taxa = table(&db, "taxon_resolutions")?;
        for row in rows {
            let (number, sample_id, attributes, taxon_id) = row?;
            let sample = local(&["sample-record", &doi, &metadata_hash, &number.to_string()]);
            graph.class(&sample, "SampleRecord")?;
            graph.property(&dataset, "hasSampleRecord", &sample)?;
            graph.text(&sample, "attributesJson", &attributes)?;
            if let Some(id) = sample_id {
                graph.text(&sample, "sampleIdentifier", &id)?;
            }
            if let Some(taxon_id) = taxon_id {
                let name: Option<String> = db
                    .query_row("SELECT name FROM taxa WHERE id=?1", [taxon_id], |r| {
                        r.get(0)
                    })
                    .ok();
                if let Some(name) = name {
                    graph.text(&sample, "reportedTaxonName", &name)?;
                }
                if has_taxa {
                    let resolved: Option<String> = db
                        .query_row(
                            "SELECT qid FROM taxon_resolutions WHERE taxon_id=?1",
                            [taxon_id],
                            |r| r.get(0),
                        )
                        .ok()
                        .flatten();
                    if let Some(qid) = resolved.as_deref().and_then(qid) {
                        graph.property(&sample, "reportedSourceTaxon", &qid)?;
                    }
                }
            }
            samples.insert(number, sample);
        }
        let file_count: i64 = db.query_row(
            "SELECT COUNT(*) FROM spectrum_files WHERE dataset_id=?1",
            [dataset_id],
            |r| r.get(0),
        )?;
        let file_progress = ProgressBar::new(u64::try_from(file_count)?);
        file_progress.set_style(ProgressStyle::with_template(
            "{spinner:.cyan} [{elapsed_precise}] [{bar:36.cyan/blue}] {pos}/{len} spectra hashed and exported",
        )?);
        file_progress.enable_steady_tick(Duration::from_millis(120));
        let has_processed = table(&db, "processed_spectra")?;
        let sql = if has_processed {
            "SELECT f.filename, f.row_number, f.relative_path, p.ms2_path, p.ms2_sha256, p.features_path, p.paired_path FROM spectrum_files f LEFT JOIN processed_spectra p ON p.dataset_id=f.dataset_id AND p.filename=f.filename WHERE f.dataset_id=?1 ORDER BY f.filename"
        } else {
            "SELECT f.filename, f.row_number, f.relative_path, NULL, NULL, NULL, NULL FROM spectrum_files f WHERE f.dataset_id=?1 ORDER BY f.filename"
        };
        let mut files = db.prepare(sql)?;
        let files = files.query_map([dataset_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })?;
        let mut processed = BTreeMap::<String, Processed>::new();
        for file in files {
            let (filename, row, relative, ms2_path, ms2_sha, audit_path, paired) = file?;
            let sample = samples
                .get(&row)
                .context("spectrum references missing sample row")?;
            let raw_sha = file_hash(&Path::new(&root).join(&relative))?;
            let raw = local(&["raw", &doi, &filename, &raw_sha]);
            let acquisition = local(&["acquisition", &doi, sample, &filename, &raw_sha]);
            graph.class(&raw, "RawFile")?;
            graph.hash(&raw, &raw_sha)?;
            graph.class(&acquisition, "Acquisition")?;
            graph.property(sample, "hasAcquisition", &acquisition)?;
            graph.used(&acquisition, sample)?;
            graph.property(&acquisition, "rawFile", &raw)?;
            graph.produced(&raw, &acquisition)?;
            graph.text(&raw, "filename", &filename)?;
            if let (Some(ms2_path), Some(ms2_sha), Some(audit_path)) =
                (ms2_path, ms2_sha, audit_path)
            {
                let actual_sha = file_hash(Path::new(&ms2_path))?;
                ensure!(
                    actual_sha == ms2_sha,
                    "registered MGF SHA-256 differs from file: {ms2_path}"
                );
                let audit_sha = file_hash(Path::new(&audit_path))?;
                let accepted = artifact::audit(Path::new(&audit_path))?;
                artifact::validate_mgf(Path::new(&ms2_path), &filename, &accepted, false)?;
                let processing = local(&[
                    "processing",
                    &doi,
                    &filename,
                    &raw_sha,
                    &actual_sha,
                    &audit_sha,
                ]);
                graph.class(&processing, "Processing")?;
                graph.used(&processing, &raw)?;
                graph.property(&processing, "derivedFrom", &raw)?;
                let ms2 = local(&["mgf", &actual_sha]);
                graph.class(&ms2, "ProcessedSpectrumFile")?;
                graph.hash(&ms2, &actual_sha)?;
                graph.produced(&ms2, &processing)?;
                graph.property(&processing, "processedSpectrumFile", &ms2)?;
                let audit_file = local(&["audit", &audit_sha]);
                graph.class(&audit_file, "FeatureAuditFile")?;
                graph.hash(&audit_file, &audit_sha)?;
                graph.produced(&audit_file, &processing)?;
                graph.property(&processing, "featureAudit", &audit_file)?;
                if let Some(paired_path) = paired {
                    let paired_sha = file_hash(Path::new(&paired_path))?;
                    artifact::validate_mgf(Path::new(&paired_path), &filename, &accepted, true)?;
                    let paired_file = local(&["mgf", &paired_sha]);
                    graph.class(&paired_file, "ProcessedSpectrumFile")?;
                    graph.hash(&paired_file, &paired_sha)?;
                    graph.produced(&paired_file, &processing)?;
                    graph.property(&processing, "pairedFile", &paired_file)?;
                }
                for (feature_id, (mz, rt)) in &accepted {
                    let feature = local(&["feature", &processing, feature_id]);
                    let query = local(&["query-spectrum", &feature]);
                    graph.class(&feature, "Feature")?;
                    graph.property(&processing, "hasFeature", &feature)?;
                    graph.text(&feature, "featureIdentifier", feature_id)?;
                    graph.typed(&feature, "precursorMz", mz, &format!("{XSD}double"))?;
                    graph.typed(
                        &feature,
                        "retentionTimeSeconds",
                        rt,
                        &format!("{XSD}double"),
                    )?;
                    graph.class(&query, "QuerySpectrum")?;
                    graph.property(&feature, "querySpectrum", &query)?;
                    graph.property(&query, "derivedFrom", &ms2)?;
                    graph.produced(&query, &processing)?;
                }
                processed.insert(
                    filename,
                    Processed {
                        uri: processing,
                        ms2_hash: actual_sha,
                        features: accepted,
                    },
                );
            }
            file_progress.inc(1);
        }
        file_progress.finish();
        let has_chemical = table(&db, "chemical_lookups")? && table(&db, "chemical_entities")?;
        if table(&db, "annotation_runs")? {
            let run_count: i64 = db.query_row(
                "SELECT COUNT(*) FROM annotation_runs WHERE dataset_id=?1",
                [dataset_id],
                |r| r.get(0),
            )?;
            let run_progress = ProgressBar::new(u64::try_from(run_count)?);
            run_progress.set_style(ProgressStyle::with_template(
                "{spinner:.cyan} [{elapsed_precise}] [{bar:36.cyan/blue}] {pos}/{len} annotation runs exported",
            )?);
            run_progress.enable_steady_tick(Duration::from_millis(120));
            let mut runs = db.prepare("SELECT id, filename, ms2_sha256, registered_ms2_sha256, library_sha256, lotus_csv_sha256, config_sha256, config_path, status, error, taxon_name, taxon_qid, taxonomy_query, matcher_version, created_at FROM annotation_runs WHERE dataset_id=?1 ORDER BY id")?;
            let runs = runs.query_map([dataset_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, String>(8)?,
                    r.get::<_, Option<String>>(9)?,
                    r.get::<_, Option<String>>(10)?,
                    r.get::<_, Option<String>>(11)?,
                    r.get::<_, Option<String>>(12)?,
                    r.get::<_, String>(13)?,
                    r.get::<_, String>(14)?,
                ))
            })?;
            let has_features = table(&db, "annotation_features")?;
            let has_hits = table(&db, "annotation_hits")?;
            for run in runs {
                let (
                    run_id,
                    filename,
                    ms2,
                    registered,
                    library,
                    lotus,
                    config,
                    config_path,
                    status,
                    error,
                    taxon_name,
                    taxon_qid,
                    taxonomy_query,
                    matcher_version,
                    created_at,
                ) = run?;
                let id = run_id.to_string();
                let uri = local(&[
                    "annotation",
                    &doi,
                    &id,
                    &filename,
                    ms2.as_deref().unwrap_or(""),
                    &library,
                    &config,
                ]);
                graph.class(&uri, "AnnotationRun")?;
                graph.text(&uri, "status", &status)?;
                if let Some(error) = error {
                    graph.text(&uri, "error", &error)?;
                }
                graph.text(&uri, "rawFilename", &filename)?;
                graph.text(&uri, "matcherVersion", &matcher_version)?;
                graph.typed(&uri, "createdAt", &created_at, &format!("{XSD}dateTime"))?;
                if let Some(name) = &taxon_name {
                    graph.text(&uri, "scoringTaxonName", name)?;
                }
                if let Some(value) = &taxon_qid {
                    if let Some(item) = qid(value) {
                        graph.property(&uri, "scoringTaxon", &item)?;
                    }
                }
                if let Some(query) = &taxonomy_query {
                    graph.text(&uri, "taxonomyQuery", query)?;
                }
                if let Some(ms2) = &ms2 {
                    graph.text(&uri, "ms2Sha256", ms2)?;
                }
                graph.text(&uri, "librarySha256", &library)?;
                graph.text(&uri, "configSha256", &config)?;
                graph.text(&uri, "lotusSha256", &lotus)?;
                for (kind, digest) in [
                    ("ReferenceLibrary", &library),
                    ("LotusSnapshot", &lotus),
                    ("SearchConfig", &config),
                ] {
                    let artifact = local(&[kind, digest]);
                    graph.class(&artifact, kind)?;
                    graph.hash(&artifact, digest)?;
                    graph.used(&uri, &artifact)?;
                }
                if let Some(ms2) = &ms2 {
                    let artifact = local(&["mgf", ms2]);
                    graph.class(&artifact, "ProcessedSpectrumFile")?;
                    graph.hash(&artifact, ms2)?;
                    graph.used(&uri, &artifact)?;
                }
                // Only hash-backed config bytes justify naming the actual metric.
                if file_hash(Path::new(&config_path)).ok().as_deref() == Some(&config) {
                    if let Ok(content) = fs::read_to_string(&config_path) {
                        if let Ok(value) = content.parse::<toml::Value>() {
                            if let Some(metric) = value
                                .get("search")
                                .and_then(|s| s.get("metric"))
                                .and_then(toml::Value::as_str)
                            {
                                graph.text(&uri, "metric", metric)?;
                            }
                        }
                    }
                }
                let current = processed.get(&filename).filter(|p| {
                    ms2.as_deref() == Some(&p.ms2_hash)
                        && registered.as_deref() == Some(&p.ms2_hash)
                });
                if let Some(p) = current {
                    graph.property(&uri, "derivedFrom", &p.uri)?;
                }
                if has_features {
                    let mut stmt = db.prepare("SELECT feature_id, precursor_mz, status FROM annotation_features WHERE run_id=?1 ORDER BY feature_id")?;
                    let features = stmt.query_map([run_id], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, f64>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    })?;
                    for feature in features {
                        let (feature_id, mz, feature_status) = feature?;
                        let run_feature = local(&["annotated-feature", &uri, &feature_id]);
                        graph.class(&run_feature, "AnnotationFeature")?;
                        graph.text(&run_feature, "status", &feature_status)?;
                        graph.text(&run_feature, "featureIdentifier", &feature_id)?;
                        graph.typed(
                            &run_feature,
                            "precursorMz",
                            &mz.to_string(),
                            &format!("{XSD}double"),
                        )?;
                        graph.property(&uri, "annotationFeature", &run_feature)?;
                        if let Some(p) = current.filter(|p| p.features.contains_key(&feature_id)) {
                            let feature = local(&["feature", &p.uri, &feature_id]);
                            graph.property(&run_feature, "feature", &feature)?;
                            graph.property(&uri, "hasFeature", &feature)?;
                            graph.property(
                                &uri,
                                "querySpectrum",
                                &local(&["query-spectrum", &feature]),
                            )?;
                        }
                    }
                }
                if has_hits {
                    let mut stmt = db.prepare("SELECT feature_id, rank, library_spectrum_id, library_inchikey, spectral_score, taxonomic_score, combined_score, rank_before_taxonomy, taxonomic_support, matches, ms1_deviation_ppm, matched_organism_name, matched_organism_wikidata, matched_shared_rank, matched_lotus_inchikey, matched_lotus_structure_wikidata, matched_lotus_reference_wikidata, matched_lotus_reference_doi FROM annotation_hits WHERE run_id=?1 ORDER BY feature_id, rank")?;
                    let hits = stmt.query_map([run_id], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, Option<String>>(3)?,
                            r.get::<_, f64>(4)?,
                            r.get::<_, f64>(5)?,
                            r.get::<_, f64>(6)?,
                            r.get::<_, Option<i64>>(7)?,
                            r.get::<_, String>(8)?,
                            r.get::<_, i64>(9)?,
                            r.get::<_, f64>(10)?,
                            r.get::<_, Option<String>>(11)?,
                            r.get::<_, Option<String>>(12)?,
                            r.get::<_, Option<String>>(13)?,
                            r.get::<_, Option<String>>(14)?,
                            r.get::<_, Option<String>>(15)?,
                            r.get::<_, Option<String>>(16)?,
                            r.get::<_, Option<String>>(17)?,
                        ))
                    })?;
                    for hit in hits {
                        let (
                            feature_id,
                            rank,
                            spectrum_id,
                            inchikey,
                            spectral,
                            taxonomic,
                            combined,
                            before_taxonomy,
                            support,
                            matches,
                            deviation_ppm,
                            organism_name,
                            organism_wikidata,
                            shared_rank,
                            matched_lotus_inchikey,
                            matched_lotus_structure_wikidata,
                            matched_lotus_reference_wikidata,
                            matched_lotus_reference_doi,
                        ) = hit?;
                        let hit_uri = local(&["hit", &uri, &feature_id, &rank.to_string()]);
                        let reference = local(&["reference", &library, &spectrum_id]);
                        graph.class(&hit_uri, "CandidateHit")?;
                        graph.property(&uri, "hasCandidateHit", &hit_uri)?;
                        graph.produced(&hit_uri, &uri)?;
                        graph.class(&reference, "ReferenceSpectrum")?;
                        graph.property(&hit_uri, "referenceSpectrum", &reference)?;
                        graph.text(&reference, "librarySpectrumIdentifier", &spectrum_id)?;
                        graph.text(&reference, "librarySha256", &library)?;
                        graph.typed(
                            &hit_uri,
                            "rank",
                            &rank.to_string(),
                            &format!("{XSD}integer"),
                        )?;
                        graph.typed(
                            &hit_uri,
                            "spectralScore",
                            &spectral.to_string(),
                            &format!("{XSD}double"),
                        )?;
                        graph.typed(
                            &hit_uri,
                            "taxonomicScore",
                            &taxonomic.to_string(),
                            &format!("{XSD}double"),
                        )?;
                        graph.typed(
                            &hit_uri,
                            "combinedScore",
                            &combined.to_string(),
                            &format!("{XSD}double"),
                        )?;
                        if let Some(rank) = before_taxonomy {
                            graph.typed(
                                &hit_uri,
                                "rankBeforeTaxonomy",
                                &rank.to_string(),
                                &format!("{XSD}integer"),
                            )?;
                        }
                        graph.text(&hit_uri, "taxonomicSupport", &support)?;
                        graph.typed(
                            &hit_uri,
                            "matchedPeakCount",
                            &matches.to_string(),
                            &format!("{XSD}integer"),
                        )?;
                        graph.typed(
                            &hit_uri,
                            "ms1DeviationPpm",
                            &deviation_ppm.to_string(),
                            &format!("{XSD}double"),
                        )?;
                        if let Some(name) = organism_name {
                            graph.text(&hit_uri, "matchedOrganismName", &name)?;
                        }
                        if let Some(value) = organism_wikidata {
                            if let Some(item) = qid(&value) {
                                graph.property(&hit_uri, "matchedOrganism", &item)?;
                            }
                        }
                        if let Some(rank) = shared_rank {
                            graph.text(&hit_uri, "matchedSharedRank", &rank)?;
                        }
                        if let Some(key) = matched_lotus_inchikey {
                            graph.text(&hit_uri, "matchedLotusInchikey", &key)?;
                        }
                        if let Some(value) = matched_lotus_structure_wikidata
                            && let Some(item) = qid(&value)
                        {
                            graph.property(&hit_uri, "matchedLotusStructure", &item)?;
                        }
                        if let Some(value) = matched_lotus_reference_wikidata
                            && let Some(item) = qid(&value)
                        {
                            graph.property(&hit_uri, "matchedLotusReference", &item)?;
                        }
                        if let Some(doi) = matched_lotus_reference_doi {
                            graph.text(&hit_uri, "matchedLotusReferenceDoi", &doi)?;
                        }
                        graph.text(&hit_uri, "featureIdentifier", &feature_id)?;
                        if let Some(p) = current.filter(|p| p.features.contains_key(&feature_id)) {
                            graph.property(
                                &hit_uri,
                                "feature",
                                &local(&["feature", &p.uri, &feature_id]),
                            )?;
                        }
                        if let Some(key) = inchikey {
                            graph.text(&hit_uri, "inchikey", &key)?;
                            if has_chemical && full_inchikey(&key) {
                                let lookup: Option<String> = db
                                    .query_row(
                                        "SELECT status FROM chemical_lookups WHERE inchikey=?1",
                                        [&key],
                                        |r| r.get(0),
                                    )
                                    .ok();
                                if lookup.as_deref() == Some("matched") {
                                    let mut stmt = db.prepare("SELECT wikidata_qid FROM chemical_entities WHERE inchikey=?1 ORDER BY wikidata_qid")?;
                                    let mapped: Vec<String> = stmt
                                        .query_map([&key], |r| r.get(0))?
                                        .collect::<rusqlite::Result<_>>()?;
                                    if let [only] = mapped.as_slice() {
                                        if let Some(qid) = qid(only) {
                                            graph.property(&hit_uri, "candidateChemical", &qid)?;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                run_progress.inc(1);
            }
            run_progress.finish();
        }
        graph.0.finish()?.flush()?;
    }
    tmp.as_file_mut().sync_all()?;
    tmp.persist_noclobber(output)
        .with_context(|| format!("output already exists: {}", output.display()))?;
    Ok(())
}

/// Execute arbitrary SPARQL SELECT against a local N-Quads file, streaming standards-compliant TSV.
pub fn query(graph: &Path, query: &Path, output: impl Write) -> Result<()> {
    let store = Store::new()?;
    store.load_from_reader(
        RdfParser::from_format(RdfFormat::NQuads),
        BufReader::new(File::open(graph)?),
    )?;
    let query = fs::read_to_string(query)?;
    match oxigraph::sparql::SparqlEvaluator::new()
        .parse_query(&query)?
        .on_store(&store)
        .execute()?
    {
        QueryResults::Solutions(solutions) => {
            let mut writer = QueryResultsSerializer::from_format(QueryResultsFormat::Tsv)
                .serialize_solutions_to_writer(output, solutions.variables().to_vec())?;
            for solution in solutions {
                writer.serialize(&solution?)?;
            }
            writer.finish()?;
            Ok(())
        }
        _ => bail!("kg query requires a SPARQL SELECT query"),
    }
}
