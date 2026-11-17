# Review and publish metadata-to-Wikidata mappings

## What is the source of truth?

| Layer | What it controls |
| --- | --- |
| Source metadata, copied unchanged into `sample_rows.attributes_json` at ingest | Which exact text each sample record reports. For this dataset: `observation_subject` and `collector_full_name`. Editing the original TSV after ingest does **not** update SQLite; re-ingest if the metadata itself changes. |
| Reviewed TSVs in [`mappings/`](mappings/) | **The maintainable, version-controlled source of truth for approved text → QID decisions.** One file per dataset and source field. The TSV must contain the **entire desired mapping for that field**, not just newly added rows. It does not alter source metadata. |
| Active `kg_mapping_imports` / `kg_mapping_decisions` in the selected SQLite DB | The **operational source of truth for `kg export`**. `kg map import` snapshots a reviewed TSV into this DB, scoped to dataset ID **and** field; old snapshots remain for audit. Editing the TSV alone does not activate it. |
| Exported graph / QLever index | A frozen projection of the DB's active decisions at export/index time. Neither updates itself when a file or DB changes. |

These are two **separate** mappings, not two rows in one file:

- [`massive-observation-subject.tsv`](mappings/massive-observation-subject.tsv): exact `Leaf` → [leaf (Q33971)](https://www.wikidata.org/wiki/Q33971). This identifies the *reported observation subject*, not the organism's taxon or a chemical in the sample.
- [`massive-collector-full-name.tsv`](mappings/massive-collector-full-name.tsv): exact `Pierre-Marie Allard` → [Pierre-Marie Allard (Q56451301)](https://www.wikidata.org/wiki/Q56451301). The MassIVE source has **14** rows with this spelling; all 14 supply ORCID `0000-0003-3389-2191`, also listed on that Wikidata item. Ten report `Leaf`; two `Stems`; two `Fruit`. The collector link applies to **all 14**, regardless of observation subject. Other spellings/people are not mapped by this decision.

The full MassIVE source metadata has 3,425 rows, including 1,207 exact `Leaf` values. These are **source-file counts**, not promises about every SQLite DB: the older top-250 subset has 250 empty observation subjects, so the Leaf mapping cannot be imported there. Confirm the actual DB and dataset before importing. Blank fields produce no link; case and whitespace must match the source exactly. Source values are never rewritten to QIDs.

## Add or change a mapping: one repeatable procedure

Run from the **flock repository root**. `DB` must point to an existing Peregrino SQLite database containing the intended imported MassIVE rows; **do not** use a toy/top-250 DB for this full-source example. Rehearse against a copy when in doubt. The import changes **only the named DB**, while graph export reads it and refuses to overwrite an existing output.

1. **Identify the dataset and inventory the ingested values.** The MassIVE DOI is `10.25345/C55T3GD90`; dataset IDs vary by DB. The inventory is taken from SQLite, not from the source TSV on disk:

   ```sh
   DB=/path/to/processed/flock.sqlite
   cargo run --release --manifest-path peregrino/Cargo.toml -- show datasets --db "$DB"
   DATASET_ID=2  # replace with the ID for the MassIVE DOI above in THIS DB
   sqlite3 "$DB" "SELECT json_extract(attributes_json, '$.observation_subject') AS value, count(*) FROM sample_rows WHERE dataset_id=$DATASET_ID GROUP BY value ORDER BY count(*) DESC;"
   sqlite3 "$DB" "SELECT json_extract(attributes_json, '$.collector_full_name') AS value, count(*) FROM sample_rows WHERE dataset_id=$DATASET_ID GROUP BY value ORDER BY count(*) DESC;"
   ```

2. **Review an exact source value and its Wikidata identity.** Look up the item and verify its meaning, not merely its label. For a person, compare a stable identifier (here the source ORCID and Wikidata ORCID). Record the exact source spelling, bare `Q...` ID, reviewer and rationale in the correct four-column TSV (`source_value`, `wikidata_qid`, `reviewer`, `rationale`, tab-separated). To add another collector, add a row to the **collector** file while keeping the Pierre-Marie row; to add another botanical subject, add it to the **subject** file while keeping `Leaf`. Distinct source spellings need distinct reviewed rows. See [subject candidate research](OBSERVATION-SUBJECT-SOURCES.md) for senses that are **not** automatically approved. A QID is syntax-checked on import, but the importer does not ask Wikidata whether it is the right entity.

3. **Import each complete reviewed file into the correct field** (both commands are required to activate both examples in a new DB):

   ```sh
   cargo run --release --manifest-path peregrino/Cargo.toml -- kg map import \
     --db "$DB" --dataset-id "$DATASET_ID" --field observation_subject \
     --file peregrino/kg/mappings/massive-observation-subject.tsv
   cargo run --release --manifest-path peregrino/Cargo.toml -- kg map import \
     --db "$DB" --dataset-id "$DATASET_ID" --field collector_full_name \
     --file peregrino/kg/mappings/massive-collector-full-name.tsv
   cargo run --release --manifest-path peregrino/Cargo.toml -- show kg_mapping_imports --db "$DB" --limit 50
   cargo run --release --manifest-path peregrino/Cargo.toml -- show kg_mapping_decisions --db "$DB" --limit 50
   ```

   Import rejects reviewed source values absent from the selected dataset. An identical active file is a no-op; an edited file becomes a new active version **for that field only**, retaining the old one. Removing a row from a reviewed file withdraws just that decision on the next import; importing a header-only file withdraws all decisions for that field. Do not delete a row accidentally when adding another. A failed import cannot replace the active snapshot. The DB records file path, SHA-256, reviewer and rationale; it does **not** watch the TSV for later edits.

4. **Export a new graph, validate and inspect it.** Choose a nonexistent graph path. The full dataset export rehashes registered/raw files and may take time. It emits original JSON plus nonempty `kg:observationSubjectText` / `kg:collectorNameText` on each `kg:SampleRecord`, and only for active reviewed matches emits `kg:reportedObservationSubject` / `kg:reportedCollector` links to Wikidata item IRIs. `kg:reportedSourceTaxon` is a separate relation. `kg:Dataset` carries `kg:observationSubjectMappingSha256` and `kg:collectorMappingSha256` for the respective active mapping snapshots.

   ```sh
   GRAPH=/path/to/new/observations-and-collectors.nq
   cargo run --release --manifest-path peregrino/Cargo.toml -- kg export \
     --db "$DB" --dataset-id "$DATASET_ID" --out "$GRAPH"
   cargo run --release --manifest-path peregrino/Cargo.toml -- kg validate --graph "$GRAPH"
   cargo run --release --manifest-path peregrino/Cargo.toml -- kg query \
     --graph "$GRAPH" --query peregrino/kg/collector-observations.rq
   cargo run --release --manifest-path peregrino/Cargo.toml -- kg query \
     --graph "$GRAPH" --query peregrino/kg/observation-subject-candidates.rq
   ```

   The [collector query](collector-observations.rq) lists the mapped collector's sample records and any **separately approved** subject. The [Leaf query](observation-subject-candidates.rq) follows acquisitions/features to ranked chemical **candidates**, not demonstrated chemical presence in a physical leaf. `SampleRecord` means a metadata row, **not** a physical specimen. Its `Acquisition` describes an associated raw-file analysis. Do not infer a subject from a taxon, `sample_type`, or a blank source value. The [local shapes](shapes.ttl) check the exported predicates.

5. **Publish only when ready.** The graph includes the original metadata JSON, which may contain personal or sensitive fields; review licensing and access before sharing it. Index this **new** graph in a new QLever backend using [Mato's helper](../../mato/TECHNICAL.md#serve-exported-graphs-with-qlever). Never overwrite a running index. A previous graph or running QLever server will not pick up either new mapping automatically. Mato's fourth managed example, [`leaf-observations.rq`](leaf-observations.rq), displays mapped Leaf sample records and associated file names even if they have no annotation hits. Editing the query file alone does not update the QLever UI: rerun `mato qlever add` for an existing backend to sync managed examples without rebuilding its index.

### Process only source rows that report an organ

The [MassIVE Mato config](../../mato/config/massive.toml) has optional top-N preselection rules: with `mato run --config config/massive.local.toml --topN 25`, it first requires nonblank `observation_subject` **and** `collector_full_name`, sorts matching source metadata by filename, and then selects 25 rows. The original TSV is not replaced; Machaca and Peregrino ingest only a generated, content-addressed snapshot. Picoto, annotation and graph export therefore process **only those selected rows and their files**; the whole MassIVE dataset need not be processed first. See the [Mato workflow](../../mato/TECHNICAL.md#example-first-25-massive-records-with-reported-organs-and-collectors) for the exact config, result paths, resume/reset rules and distinction between metadata-row and raw-file counts. This is independent of the reviewed `Leaf` → Q33971 decision: a reported but unmapped `Stems` remains in the selected data without inventing a QID. Import curated mappings into the selected job's DB before exporting its graph.
