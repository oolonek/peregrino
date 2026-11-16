# Peregrino technical guide

[Back to the Peregrino overview](README.md).

## Ingest validated dataset metadata

Peregrino imports validated sample metadata and mass-spectrometry file references into SQLite, and explicitly registers picoto-produced MS2 MGF files. The first supported ingestion flow uses an extracted sapayoa dataset and machaca's filename validation; peregrino does not fetch, extract, or parse raw spectra.


```sh
# From the flock directory (parent of peregrino/); supply a dataset root containing these relative files.
DATASET_ROOT=/path/to/extracted/zenodo-10018590
DB=/path/to/flock.sqlite
SELECTION=/path/to/dataset-selection.toml
cargo run --manifest-path peregrino/Cargo.toml -- ingest \
  "$DATASET_ROOT" \
  --doi 10.5281/zenodo.10018264 \
  --metadata metadata/metadata.tsv \
  --ms-dir msdata/raw \
  --config "$SELECTION" \
  --db "$DB"
```

`--config` is required. If the file already exists, it must be TOML containing the exact metadata header to use as the taxon name, for example:

```toml
[dataset]
taxon_column = "taxon_scientific_name"
```

Other TOML sections are allowed, including in Mato's own config file. If the file does not exist, Peregrino lists **all** metadata headers with numbers and asks you to choose one; it atomically creates a minimal config at the supplied path and uses that choice on future runs. Choose a config path outside the source dataset unless you explicitly want it there. Missing, empty, or nonexact headers and invalid or unavailable operator input stop before any database changes; there is no fallback or automatic taxon-column priority.

Paths for `--metadata` and `--ms-dir` must resolve inside the extracted dataset. The database path may be outside it; peregrino creates missing parent directories after validating the input. Directory creation or SQLite opening still fails if the destination is not writable. `--verbose` logs each linked spectrum; otherwise a terminal spinner counts sample rows. Machaca must find exactly one row reference for every `.mzML`, `.mzXML`, or `.mzData` file (including `.gz` variants) in the selected directory. Import stops before changing the database if validation fails.

To rehearse the original MassIVE `MSV000102993` dataset without copying or
curating its metadata or raw spectra, use `--max-rows 100` and a separate SQLite
database path:

```sh
# Choose an extracted copy of MSV000102993 and a separate rehearsal database.
DATASET_ROOT=/path/to/extracted/MSV000102993
REHEARSAL_DB=/path/to/rehearsal/flock.sqlite
SELECTION=/path/to/dataset-selection.toml
cargo run --manifest-path peregrino/Cargo.toml -- ingest \
  "$DATASET_ROOT" \
  --doi 10.25345/C55T3GD90 \
  --metadata metadata/metadata/dbgi_massive_metadata_20260806.tsv \
  --ms-dir peak/peak \
  --config "$SELECTION" \
  --db "$REHEARSAL_DB" \
  --max-rows 100
```

The limit selects the first
100 data records in source order (not a hand-picked subset): only those sample
rows and their referenced raw-file paths enter SQLite. Unselected physical
files and metadata rows are ignored, but invalid filenames, duplicate
references, and missing or ambiguous raw matches among selected rows still
fail. `N` must be greater than zero. Without `--max-rows`, every metadata
record and every spectrum in the directory must match, unchanged from the
normal full import. Reimporting the same DOI and source root replaces its
existing rows even when the limit changes; use a separate database for the
rehearsal to keep a full-run database untouched.

## Register picoto spectra

After `ingest`, run picoto with `--mode ms2` or `--mode both`, then register each raw file's MS2-only MGF and feature audit:

```sh
cargo run --manifest-path peregrino/Cargo.toml -- register \
  --db /path/to/flock.sqlite \
  --dataset /path/to/extracted-dataset \
  --raw run.mzXML \
  --ms2 /path/to/picoto-output/run_ms2.mgf \
  --features /path/to/picoto-output/run_features.tsv \
  --paired /path/to/picoto-output/run_sirius.mgf
```

`--paired` is optional. The database must already exist and contain exactly one ingested dataset for the canonical `--dataset` root and the named raw filename. Outputs may live outside the dataset but must be regular files together in one directory, with picoto's matching output names. Registration validates the audit's accepted feature IDs against unique `FEATURE_ID` blocks, original `FILENAME`, picoto `TITLE`, and MS level in each MGF; paired output must contain both MS1 and MS2 for the same features. This refuses unrelated MZmine exports and mismatched picoto runs, though outputs do not cryptographically prove which raw file picoto read. Failed validation does not create or update a registration. `ms2_sha256` is the lowercase SHA-256 digest of the registered MS2 MGF; consumers should compare the current file digest before using its annotations.

Registration and `kg export` use the same Picoto audit/MGF validation: accepted feature IDs must match unique MS2 blocks (and MS1 blocks in a paired file), and precursor m/z and retention time must agree within Picoto's decimal rounding (0.00000051 m/z, 0.00055 seconds). Export still re-reads the files and checks the registered MS2 hash; older registrations with incomplete or inconsistent Picoto files need fresh processing and registration before export.

The `processed_spectra` table stores canonical absolute `ms2_path`, `features_path`, optional `paired_path`, `ms2_sha256`, UTC `registered_at`, and `raw_identity` under its `(dataset_id, filename)` key referencing `spectrum_files`. Existing ingested databases gain the table on registration (or next ingest). Re-ingesting preserves a registration when the raw filename, relative path, and filesystem identity (canonical path, device, inode, size, modification time) remain unchanged; deleted/moved/replaced raw references lose it. This stat-based identity is intentionally cheaper than rehashing potentially large raw spectra; an in-place rewrite restoring all these metadata values cannot be detected and should be followed by fresh picoto processing and registration.

Concurrent `register` processes validate and hash their own files in parallel. SQLite still has one writer: each process acquires an immediate write reservation before reading registration rows and waits up to 30 seconds for a competing writer. This prevents competing read-to-write upgrades from failing immediately with `database is locked`; a long-running unrelated writer can still exceed the timeout.

## Inspect and export (T001.01)

From the `peregrino` crate directory, inspect the database created by the ingest command:

```sh
# From the peregrino crate directory; choose the ingested SQLite file.
DB=/path/to/flock.sqlite
cargo run -- tables --db "$DB"
cargo run -- show spectrum_files --db "$DB"
cargo run -- show sample_rows --db "$DB" --limit 50
cargo run -- show taxa --db "$DB"
cargo run -- export spectrum_files --db "$DB" --out /path/to/new/spectrum_files.tsv
```

`tables` lists non-internal SQLite tables and views (under the `relation` header), including hocofaisan's annotation status views. `show` writes headers and up to 20 rows by default as TSV on stdout (`--limit` changes the preview size). `export` writes all rows from a table or view to a **new** TSV file; it creates missing parent directories but refuses to overwrite an existing file. Quoting follows TSV/CSV rules, including embedded tabs and newlines; SQL `NULL` becomes an empty field and binary values use `0x`-prefixed hexadecimal. The SQLite database is opened read-only for these commands, and a missing database or unknown relation is an error. `sample_rows.attributes_json` retains the original metadata fields as JSON.

### Explore annotation results

Peregrino can inspect Hocofaisan results without modifying the database:

```sh
cargo run --manifest-path peregrino/Cargo.toml -- annotations --db /path/to/flock.sqlite summary
cargo run --manifest-path peregrino/Cargo.toml -- annotations --db /path/to/flock.sqlite files --status hits --limit 20
cargo run --manifest-path peregrino/Cargo.toml -- annotations --db /path/to/flock.sqlite counts --dataset-id 2 --min-taxonomic-score 9 --shared-rank species --limit 20
cargo run --manifest-path peregrino/Cargo.toml -- annotations --db /path/to/flock.sqlite hits --dataset-id 2 --file run.mzML --min-spectral-score 0.7 --support supported
```

The `features` command includes spectra with no hits; `hits` shows per-feature ranks, spectral and taxonomic scores, matched organism, and compound identifiers. `errors` lists current failures. For filters, interpretation, and copyable read-only SQL recipes—including top species-supported hits across files—see [ANNOTATIONS.md](ANNOTATIONS.md). A current run matches its registered Picoto MS2 hash; this does not establish that its library and scoring config still match files on disk.

## Wikidata-linked local knowledge graph

For a persistent, localhost-only SPARQL service and a separate **graphical QLever UI**, see the [portable QLever workflow](kg/QLEVER.md#export-and-index-a-graph). It uses the same N-Quads export and the repository's [Qleverfile](kg/Qleverfile), binds the engine and UI to `127.0.0.1`, and explains HTTP queries, SSH access, refresh and stop procedures. The QLever instructions distinguish the observed toy deployment from steps not exercised on another host. Use the in-process `kg query` for one-off checks; QLever indexes the graph for repeated or interactive querying.

From the flock directory (the parent of `peregrino/`), export and query an already-processed toy database (choose a **new** graph filename for each export). Look up the dataset ID for the toy DOI in this database; it is not necessarily `1`. For a fresh job including taxon and chemical reconciliation, [follow Mato's job instructions](https://github.com/oolonek/mato/blob/master/TECHNICAL.md#run-a-job): copy and edit its portable config templates into local `*.local.toml` job and Wikidata settings, set both shared index paths there, and run the job before export. The tracked templates are not a ready-made workstation deployment. Do not make per-key WDQS calls merely to export an existing graph.

```sh
DB=/path/to/processed/toy.sqlite
GRAPH=/path/to/new/toy-kg.nq
cargo run --manifest-path peregrino/Cargo.toml -- show datasets --db "$DB"
printf 'Dataset ID for the toy DOI: '
read -r DATASET_ID

cargo run --manifest-path peregrino/Cargo.toml -- kg export --db "$DB" --dataset-id "$DATASET_ID" --out "$GRAPH"
cargo run --manifest-path peregrino/Cargo.toml -- kg validate --graph "$GRAPH"
cargo run --manifest-path peregrino/Cargo.toml -- kg query --graph "$GRAPH" --query peregrino/kg/coverage.rq
cargo run --manifest-path peregrino/Cargo.toml -- kg query --graph "$GRAPH" --query peregrino/kg/toy-candidates.rq
cargo run --manifest-path peregrino/Cargo.toml -- kg query --graph "$GRAPH" --query peregrino/kg/taxonomic-evidence.rq
```

`kg export` streams deterministic N-Quads for one dataset from a consistent **read-only** SQLite snapshot and the registered files. It refuses to overwrite an existing graph. `kg query` runs a local SPARQL `SELECT` over the export and prints TSV; it loads the graph into memory, so start with a bounded dataset. [SHACL shapes](kg/shapes.ttl) constrain the local evidence graph; [coverage.rq](kg/coverage.rq) counts sample records, acquisitions, extracted features, run-scoped query observations, historical hits, and unambiguously Wikidata-linked candidates. [toy-candidates.rq](kg/toy-candidates.rq) follows taxon → sample record → acquisition → raw file → Picoto feature → scored candidate and requires an unambiguous chemical QID and spectral score ≥ 0.7. It returns both the annotation-run and hit identities: two historical runs can produce separate candidate results for one feature.

In a terminal, `kg export` displays separate progress bars for spectrum files (including raw and registered-file hashing and feature validation) and annotation runs (including their features and candidate hits). Each counter advances only after its entire file or run is exported, so a large file or a hit-heavy run can hold the counter while the spinner and elapsed time continue. Progress goes to stderr, never into the N-Quads graph. An export already running from an older binary will not acquire progress; let it finish, or rerun after rebuilding with a new output filename.

SHACL validation is **optional** and separate from building or querying the graph. `peregrino kg validate --graph "$GRAPH"` uses [Rudof's Rust SHACL library](https://rudof-project.github.io/rudof/) against the bundled [local shapes](kg/shapes.ttl), prints the validation report and exits nonzero on a malformed or nonconforming graph. It reads the N-Quads into memory; allow enough RAM and time for a multi-gigabyte export (the MassIVE top-250 graph reached 4 GB), or skip this optional check and [index it with QLever](kg/QLEVER.md). The in-process `kg query` also loads the entire graph into memory and is intended for bounded graphs. Export itself already checks registered file hashes and feature/MGF consistency. Rudof's raw CLI is not used here because it returns success even when its report contains violations; its endpoint backend against the current QLever version reports normalized numeric datatypes (`xsd:int`/`xsd:decimal`) instead of the graph's original `xsd:integer`/`xsd:double`. [Sophia](https://github.com/pchampin/sophia_rs) offers RDF toolkit primitives, but adds no necessary SHACL validation capability here because Peregrino already uses Oxigraph for RDF.

In a terminal, validation shows byte progress while loading N-Quads, then a spinner and elapsed time during SHACL checks; the second phase has no meaningful percentage. The Rust validator was exercised on the toy graph, not timed on the 4 GB top-250 graph.

[taxonomic-evidence.rq](kg/taxonomic-evidence.rq) exposes the read-only candidate path from dataset DOI and sample taxon through raw/processed files, m/z and retention time to the matcher version, historical scoring taxon, matched reference organism, scores, candidate chemical QID, and the **specific LOTUS biosource row selected for taxonomic support**: its full structure InChIKey, structure QID, and reference QID/DOI when recorded. It joins only historical runs whose input MGF is the current registered processing output; unresolved or historical pre-migration support fields remain optional rather than invented.

A sample record is metadata, **not** a physical specimen or an instrument run. One `Acquisition` represents one raw file associated with the record; one `Processing` consumes that file and produces the registered MGF and feature audit. The export hashes raw, MGF, and audit bytes; checks the registered MGF hash and each accepted audit feature's m/z and retention time against its MGF block; and identifies features within their particular processing output. Its sample-record IDs use the **ingested database rows**, not the mutable metadata TSV on disk. Picoto settings/version and a registration-time audit hash are not stored, so the graph cannot establish them retroactively. Reference-library spectra are scoped by library hash. Historical runs, errors, zero-hit queries, and candidate ranks remain distinct; a stale run's hit is **not** linked to the current Picoto feature. Each annotation run retains the recorded matcher version, creation time, scoring taxon and taxonomy query; each hit retains its matched reference organism (QID when recorded), shared taxonomic rank, support, matched-peak count and signed MS1 mass error. **Candidate chemical**, **supporting LOTUS structure**, **scoring taxon**, **matched LOTUS organism**, and **sample source taxon** are distinct roles: a short-InChIKey match can boost an unspecified-stereochemistry candidate using a different stereoisomer's occurrence. The KG attaches the recorded supporting structure and citation to the *hit*, never a `found in taxon` statement on the candidate item. Spectral and combined scores are not identification probabilities; a candidate link is not a molecule-presence claim.

Known Wikidata taxa and unambiguous full-InChIKey chemicals are linked by their QID IRIs. The local Dataset uses the existing DOI property `wdt:P356`, but the database does not record a dataset DOI→QID reconciliation; **no dataset QID is fabricated**. A LOTUS reference QID/DOI is exported **only when its selected biosource row supplies it**; unrelated literature QIDs, a physical-sample identity, and valid deposited spectrum USIs cannot be inferred from the current database. The graph uses only RDF, [PROV-O](https://www.w3.org/TR/prov-o/), Wikidata Q/P IRIs where appropriate, and the local `urn:mato:term:` vocabulary. Original metadata JSON is carried in the graph: review licensing, access and sensitive fields before publishing an export. See [Cunaguaro's mapping proposal](https://github.com/oolonek/cunaguaro/blob/master/TECHNICAL.md#local-kg-mapping-proposal) and [ontology research](https://github.com/oolonek/cunaguaro/blob/master/ONTOLOGY-RESEARCH.md) for the Wikidata/notability boundary.

For a proposed, human-reviewed mapping from `sample_rows.attributes_json.observation_subject` to Wikidata botanical concepts, including [leaf (Q33971)](https://www.wikidata.org/wiki/Q33971), see [observation-subject mapping](kg/OBSERVATION-SUBJECT.md#technical-design-proposal-not-the-current-graph). Current KG export preserves this field only inside `kg:attributesJson`; it does not yet emit individual observation-subject links.

## SQLite tables

- `datasets`: DOI and canonical dataset root (unique together), plus the metadata file and spectrum directory paths used for the current import.
- `sample_rows`: one-based source data row number, optional `sample_id`, nullable `taxon_id` referencing `taxa.id`, and all original tabular fields in `attributes_json` (SQLite JSON1 compatible).
- `spectrum_files`: one row per validated filename, linked to its source row and column; `relative_path` is relative to the dataset root. Its primary key prevents duplicate references.
- `processed_spectra`: one registered picoto output per ingested raw filename, with MS2 MGF digest, feature audit, optional paired MGF, registration time, and raw filesystem identity; its composite foreign key removes registrations when raw references disappear.
- `taxa`: distinct, nonempty taxon names (`id`, `name`) read only from the configured exact metadata header. Leading/trailing whitespace is removed from stored names; every original source value remains unchanged in `attributes_json`. A blank value in the selected column leaves `sample_rows.taxon_id` NULL, as for QC and blank samples; an empty `sample_id` also becomes NULL. Names are deduplicated across datasets, keep their IDs on re-ingestion while referenced, and are removed only when no sample row refers to them. Existing databases gain the nullable link automatically on the next ingest.

Cunaguaro reads `taxa.id, taxa.name` and writes lookup outcomes to `taxon_resolutions(taxon_id, qid, catalogue_of_life_id, error)` in the same database. Join through `sample_rows.taxon_id` to associate resolved identifiers with samples. Names in `taxa` are **not** validated identifiers; see [Cunaguaro technical guide](https://github.com/oolonek/cunaguaro/blob/master/TECHNICAL.md#shared-taxon-index-and-reconciliation) for the lookup contract and command. Peregrino's inspection commands can display or export `taxon_resolutions` after cunaguaro has run.

A rerun for the same DOI and root atomically replaces that dataset's rows and references, preserving valid `processed_spectra` links as described above. Other datasets in the same SQLite file are left untouched. Input validation or SQL errors roll back the replacement. Paths and the DOI are provenance labels supplied by the operator; they do not verify the downloaded archive's DOI or hash. The database stores metadata, not spectra or sample file contents. mzmlwatcher's database remains independent.

Run tests with `cargo test --manifest-path peregrino/Cargo.toml` from the flock directory (the parent of `peregrino/`).
