# peregrino

Peregrino imports validated sample metadata and mass-spectrometry file references into SQLite, and explicitly registers picoto-produced MS2 MGF files. The first supported ingestion flow uses an extracted sapayoa dataset and machaca's filename validation; peregrino does not fetch, extract, or parse raw spectra.

```sh
cargo run --manifest-path peregrino/Cargo.toml -- ingest \
  /media/nvme8tb/isla-gabriela/sapayoa/zenodo-10018590 \
  --doi 10.5281/zenodo.10018264 \
  --metadata metadata/metadata.tsv \
  --ms-dir msdata/raw \
  --db /path/to/flock.sqlite
```

Paths for `--metadata` and `--ms-dir` must resolve inside the extracted dataset. The database path may be outside it; peregrino creates missing parent directories after validating the input. Directory creation or SQLite opening still fails if the destination is not writable. `--verbose` logs each linked spectrum; otherwise a terminal spinner counts sample rows. Machaca must find exactly one row reference for every `.mzML`, `.mzXML`, or `.mzData` file (including `.gz` variants) in the selected directory. Import stops before changing the database if validation fails.

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

The `processed_spectra` table stores canonical absolute `ms2_path`, `features_path`, optional `paired_path`, `ms2_sha256`, UTC `registered_at`, and `raw_identity` under its `(dataset_id, filename)` key referencing `spectrum_files`. Existing ingested databases gain the table on registration (or next ingest). Re-ingesting preserves a registration when the raw filename, relative path, and filesystem identity (canonical path, device, inode, size, modification time) remain unchanged; deleted/moved/replaced raw references lose it. This stat-based identity is intentionally cheaper than rehashing potentially large raw spectra; an in-place rewrite restoring all these metadata values cannot be detected and should be followed by fresh picoto processing and registration.

## Inspect and export (T001.01)

From the `peregrino` crate directory, inspect the database created by the ingest command:

```sh
cargo run -- tables --db /media/nvme8tb/isla-gabriela/peregrino/flock.sqlite
cargo run -- show spectrum_files --db /media/nvme8tb/isla-gabriela/peregrino/flock.sqlite
cargo run -- show sample_rows --db /media/nvme8tb/isla-gabriela/peregrino/flock.sqlite --limit 50
cargo run -- show taxa --db /media/nvme8tb/isla-gabriela/peregrino/flock.sqlite
cargo run -- export spectrum_files --db /media/nvme8tb/isla-gabriela/peregrino/flock.sqlite --out /tmp/spectrum_files.tsv
```

`tables` lists non-internal SQLite tables and views (under the `relation` header), including hocofaisan's annotation status views. `show` writes headers and up to 20 rows by default as TSV on stdout (`--limit` changes the preview size). `export` writes all rows from a table or view to a **new** TSV file; it creates missing parent directories but refuses to overwrite an existing file. Quoting follows TSV/CSV rules, including embedded tabs and newlines; SQL `NULL` becomes an empty field and binary values use `0x`-prefixed hexadecimal. The SQLite database is opened read-only for these commands, and a missing database or unknown relation is an error. `sample_rows.attributes_json` retains the original metadata fields as JSON.

## SQLite tables

- `datasets`: DOI and canonical dataset root (unique together), plus the metadata file and spectrum directory paths used for the current import.
- `sample_rows`: one-based source data row number, optional `sample_id`, nullable `taxon_id` referencing `taxa.id`, and all original tabular fields in `attributes_json` (SQLite JSON1 compatible).
- `spectrum_files`: one row per validated filename, linked to its source row and column; `relative_path` is relative to the dataset root. Its primary key prevents duplicate references.
- `processed_spectra`: one registered picoto output per ingested raw filename, with MS2 MGF digest, feature audit, optional paired MGF, registration time, and raw filesystem identity; its composite foreign key removes registrations when raw references disappear.
- `taxa`: distinct, nonempty taxon names (`id`, `name`). Taxon columns take precedence in this order: `source_taxon` (the Zenodo toy dataset), `taxon_name`, `scientific_name`, then MassIVE's `taxon_scientific_name` (case-insensitive column names). The first present column is used; a blank value in that column does not fall through to another column. Leading/trailing whitespace is removed from stored names; every original source value remains unchanged in `attributes_json`. An absent column or blank value leaves `sample_rows.taxon_id` NULL, as for QC and blank samples; an empty `sample_id` also becomes NULL. Names are deduplicated across datasets, keep their IDs on re-ingestion while referenced, and are removed only when no sample row refers to them. Existing databases gain the nullable link automatically on the next ingest.

Cunaguaro reads `taxa.id, taxa.name` and writes lookup outcomes to `taxon_resolutions(taxon_id, qid, catalogue_of_life_id, error)` in the same database. Join through `sample_rows.taxon_id` to associate resolved identifiers with samples. Names in `taxa` are **not** validated identifiers; see [`cunaguaro/README.md`](../cunaguaro/README.md) for the lookup contract and command. Peregrino's inspection commands can display or export `taxon_resolutions` after cunaguaro has run.

A rerun for the same DOI and root atomically replaces that dataset's rows and references, preserving valid `processed_spectra` links as described above. Other datasets in the same SQLite file are left untouched. Input validation or SQL errors roll back the replacement. Paths and the DOI are provenance labels supplied by the operator; they do not verify the downloaded archive's DOI or hash. The database stores metadata, not spectra or sample file contents. mzmlwatcher's database remains independent.

Run tests with `cargo test --manifest-path peregrino/Cargo.toml` from the parent directory.
