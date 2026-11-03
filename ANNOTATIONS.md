# Explore Hocofaisan annotations

Open the existing Peregrino database with the SQLite CLI (no database writes):

```sh
sqlite3 -readonly -header -column /path/to/flock.sqlite
```

Paste each SQL statement below at the `sqlite>` prompt. For the queries scoped to one dataset/file, set CLI parameters first (replace the example values with a `spectrum_files.dataset_id` and `spectrum_files.filename` from your database):

```text
.parameter init
.parameter set :dataset_id 1
.parameter set :filename 'example.mgf'
```

For large databases, start with a single dataset/file. `LIMIT` bounds returned rows, **not** work done to sort, group, or rank millions of hits. Queries across all files, including the top-ten and duplicate-structure recipes, can require substantial scans. These recipes read the database; the CLI parameters are temporary session bindings, not changes to the flock database.

## Peregrino CLI

`annotations` emits TSV. `files` shows all ingested filenames and their total retained hits; `counts` counts *retained hit rows* meeting your score/taxonomy filters per current successful file (including zero-match files), with a distinct count of features having matching hits. `counts` requires `--dataset-id` or exact `--file` because it scans scored rows; a `--limit` only limits returned files, not the scan. `features` includes zero-hit query spectra; `hits` shows individual reranked candidates for an exact file; `errors` shows current failures.

```sh
DB=/media/nvme8tb/isla-gabriela/peregrino/flock.sqlite
cargo run --manifest-path peregrino/Cargo.toml -- annotations --db "$DB" summary --dataset-id 2
cargo run --manifest-path peregrino/Cargo.toml -- annotations --db "$DB" files --dataset-id 2 --status hits --limit 20
cargo run --manifest-path peregrino/Cargo.toml -- annotations --db "$DB" counts --dataset-id 2 --min-spectral-score 0.7 --min-taxonomic-score 9 --shared-rank species --limit 20
cargo run --manifest-path peregrino/Cargo.toml -- annotations --db "$DB" features --dataset-id 2 --file run.mzML --limit 20
cargo run --manifest-path peregrino/Cargo.toml -- annotations --db "$DB" hits --dataset-id 2 --file run.mzML --support supported --min-matches 6 --limit 20
cargo run --manifest-path peregrino/Cargo.toml -- annotations --db "$DB" errors --dataset-id 2 --limit 20
```

`--file` matches the raw filename exactly, not a path or a pattern; provide `--dataset-id` when the same name can occur in several datasets. `--support supported` means a positive taxonomic bonus at **any** rank; `--shared-rank species` specifically requires species-level support. `hit_count`/`matching_hits` count retained scored candidate rows, not distinct molecules.

## What a row means

- `spectrum_files` has one ingested raw filename per `(dataset_id, filename)`; `processed_spectra` holds its registered Picoto MS2 output, if any. `annotation_runs` records Hocofaisan attempts and their errors. `annotation_current_runs` selects the **latest run whose recorded MS2 SHA-256 and registered MS2 SHA-256 both equal the currently registered Picoto MS2 SHA-256** for that file. It does **not** check whether today's library, LOTUS CSV, matcher version, sample taxon, or configuration still matches that run. A missing registration or changed registered MS2 hash can remove a historical result from this view. This view does not check whether the registered file still exists on disk.
- A run has `status = 'hits'`, `'no_hits'`, or `'error'`. Successful runs record one `annotation_features` row per accepted query feature, **including zero-hit features**; error runs have no feature/hit rows. `annotation_hits` has one candidate library-spectrum match per `(run_id, feature_id, rank)`. Count files, features, and hits separately: a feature can have several hits, a file can have several features, and repeated hits or short InChIKeys are not independently confirmed compounds.
- `spectral_score` is spectral similarity, `taxonomic_score` is a LOTUS-derived bonus for the deepest shared rank (zero without positive taxonomic support), and `combined_score = spectral_score + taxonomic_score` drives reranking. The bonus is 1–10 for shared ranks domain through varietas (species = 9; varietas = 10). The three scores are **not interchangeable probabilities**. `taxonomic_support = 'supported'` means the bonus is positive; it does not necessarily mean *species*-level support. `matched_shared_rank IN ('species', 'varietas')` is the stricter species-level filter. This indicates a shared LOTUS biosource lineage for the candidate's short InChIKey and the resolved sample taxon, **not proof of chemical identity or presence in the sample**.
- `rank_before_taxonomy` is the original spectral rank; `rank` is the final, one-based rank **within one query feature**, not within a file. A positive `rank_before_taxonomy - rank` means that retained hit moved up after taxonomic reranking. `matched_short_inchikey` is a structure-level grouping key when available; distinct library spectra/features can share it, and short keys do not distinguish every full InChIKey (for example stereochemical variants).

## Latest available status, including unregistered files

One row per status combination; `latest_attempt_status` is the most recent attempt *regardless of MS2 hash*, and may be stale. `current_status` distinguishes the eligible current run from that historical attempt. Missing registration is shown even when a historical run exists.

```sql
WITH files AS (
    SELECT f.dataset_id, f.filename, p.ms2_sha256 AS registered_sha256,
           c.status AS current_status,
           last.status AS latest_attempt_status
    FROM spectrum_files AS f
    LEFT JOIN processed_spectra AS p
      ON p.dataset_id = f.dataset_id AND p.filename = f.filename
    LEFT JOIN annotation_current_runs AS c
      ON c.dataset_id = f.dataset_id AND c.filename = f.filename
    LEFT JOIN annotation_runs AS last
      ON last.id = (
          SELECT a.id FROM annotation_runs AS a
          WHERE a.dataset_id = f.dataset_id AND a.filename = f.filename
          ORDER BY a.id DESC LIMIT 1
      )
)
SELECT CASE WHEN registered_sha256 IS NULL THEN 'missing_registration'
            WHEN current_status IS NULL THEN 'no_current_annotation'
            ELSE current_status END AS file_state,
       COALESCE(latest_attempt_status, 'no_attempt') AS latest_attempt_status,
       COUNT(*) AS files
FROM files
GROUP BY file_state, latest_attempt_status
ORDER BY file_state, latest_attempt_status;
```

`no_current_annotation` can mean no attempt or only stale attempts; look at `latest_attempt_status` and the error recipe below. An `error` run can itself be current by MS2 hash; errors with no recorded MS2 hash cannot be current.

## Per-file feature/hit counts and errors

Filter by `:dataset_id` before reading per-run features. Separate aggregates avoid multiplying features by hits; a `no_hits` run retains its zero-hit features. Every ingested file in the selected dataset appears, even with no registration or eligible run.

```sql
SELECT f.dataset_id, f.filename,
       CASE WHEN p.ms2_sha256 IS NULL THEN 'missing_registration'
            WHEN r.id IS NULL THEN 'no_current_annotation'
            ELSE r.status END AS file_state,
       r.id AS run_id, r.created_at, r.query_count,
       COALESCE((SELECT COUNT(*) FROM annotation_features AS af
                 WHERE af.run_id = r.id), 0) AS features,
       COALESCE((SELECT COUNT(*) FROM annotation_features AS af
                 WHERE af.run_id = r.id AND af.status = 'no_hits'), 0) AS zero_hit_features,
       COALESCE((SELECT COUNT(*) FROM annotation_hits AS h
                 WHERE h.run_id = r.id), 0) AS hits,
       r.error
FROM spectrum_files AS f
LEFT JOIN processed_spectra AS p
  ON p.dataset_id = f.dataset_id AND p.filename = f.filename
LEFT JOIN annotation_current_runs AS r
  ON r.dataset_id = f.dataset_id AND r.filename = f.filename
WHERE f.dataset_id = :dataset_id
ORDER BY f.filename;
```

`query_count` is the matcher's accepted query count on success; recorded `error` runs have `query_count = 0`. The error column is empty for a file with no eligible run; inspect the latest attempt below before concluding that it was never attempted.

## Scores and taxonomic support distribution

A *hit-row* distribution for one current file, not a feature or compound count. The spectral bins are **< 0**, **[0, 0.25)**, **[0.25, 0.5)**, **[0.5, 0.75)**, **[0.75, 1)**, and **>= 1** (left inclusive, right exclusive). The final bucket also catches any values above 1; don't assume normalized combined scores. `matched_shared_rank` distinguishes broad lineage support from species/varietas support. An unsupported hit can still have a parsed short InChIKey.

```sql
SELECT CASE WHEN h.spectral_score < 0 THEN '<0'
            WHEN h.spectral_score < 0.25 THEN '[0,0.25)'
            WHEN h.spectral_score < 0.5 THEN '[0.25,0.5)'
            WHEN h.spectral_score < 0.75 THEN '[0.5,0.75)'
            WHEN h.spectral_score < 1 THEN '[0.75,1)'
            ELSE '>=1' END AS spectral_bin,
       h.taxonomic_support,
       COALESCE(h.matched_shared_rank, 'no_shared_rank') AS shared_rank,
       h.taxonomic_score,
       COUNT(*) AS hit_rows,
       MIN(h.combined_score) AS min_combined_score,
       MAX(h.combined_score) AS max_combined_score
FROM annotation_current_runs AS r
JOIN annotation_hits AS h ON h.run_id = r.id
WHERE r.dataset_id = :dataset_id AND r.filename = :filename
GROUP BY spectral_bin, h.taxonomic_support, shared_rank, h.taxonomic_score
ORDER BY h.taxonomic_score DESC, spectral_bin, shared_rank;
```

A file with no hits yields no groups; check the per-file feature counts rather than treating absent distribution rows as absent features.

## Top 10 species-supported hits per file across a dataset

This is the README's per-file top-ten recipe, scoped to `:dataset_id`. `file_hit_rank` orders candidate **rows across all features in that file** by combined score; it is distinct from either within-feature rank. Files with current `no_hits` results, or `hits` results without species-supported hits, appear once with empty hit columns. Files lacking a current run and current errors are intentionally excluded.

```sql
WITH ranked_species_hits AS (
    SELECT h.run_id, h.feature_id, h.rank_before_taxonomy,
           h.rank AS rank_after_taxonomy, h.library_spectrum_id,
           h.library_inchikey, h.matched_organism_name,
           h.matched_shared_rank, h.spectral_score, h.taxonomic_score,
           h.combined_score,
           ROW_NUMBER() OVER (
               PARTITION BY h.run_id
               ORDER BY h.combined_score DESC, h.spectral_score DESC,
                        h.feature_id, h.rank
           ) AS file_hit_rank
    FROM annotation_current_runs AS r
    JOIN annotation_hits AS h ON h.run_id = r.id
    WHERE r.dataset_id = :dataset_id AND r.status = 'hits'
      AND h.matched_shared_rank IN ('species', 'varietas')
)
SELECT r.dataset_id, r.filename, r.taxon_name, r.taxon_qid,
       r.status, s.file_hit_rank, s.feature_id,
       s.rank_before_taxonomy, s.rank_after_taxonomy,
       s.library_spectrum_id, s.library_inchikey,
       s.matched_organism_name, s.matched_shared_rank,
       s.spectral_score, s.taxonomic_score, s.combined_score
FROM annotation_current_runs AS r
LEFT JOIN ranked_species_hits AS s
  ON s.run_id = r.id AND s.file_hit_rank <= 10
WHERE r.dataset_id = :dataset_id AND r.status IN ('hits', 'no_hits')
ORDER BY r.filename, s.file_hit_rank;
```

## Inspect a feature's top reranked candidates

Change `h.rank <= 5` to show more candidates. The `LEFT JOIN` and rank condition in `ON` retain zero-hit features; `NULL` hit columns mean no candidate, not rank zero. Use `AND af.feature_id = '...your feature id...'` to narrow to one feature, or `LIMIT 100` to preview a file. Feature IDs are meaningful together with the run ID.

```sql
SELECT r.dataset_id, r.filename, af.run_id, af.feature_id,
       af.precursor_mz, af.status AS feature_status, af.hit_count,
       h.rank_before_taxonomy, h.rank AS rank_after_taxonomy,
       h.rank_before_taxonomy - h.rank AS positions_moved_up,
       h.library_spectrum_id, h.library_name, h.library_inchikey,
       h.spectral_score, h.taxonomic_score, h.combined_score,
       h.taxonomic_support, h.matched_shared_rank
FROM annotation_current_runs AS r
JOIN annotation_features AS af ON af.run_id = r.id
LEFT JOIN annotation_hits AS h
  ON h.run_id = af.run_id AND h.feature_id = af.feature_id
 AND h.rank <= 5
WHERE r.dataset_id = :dataset_id AND r.filename = :filename
ORDER BY af.feature_id, h.rank;
```

## Frequently repeated short InChIKeys

Counts are hit rows, distinct query features *per file*, and files respectively—not unique compounds. Count only hits with an available short key; an unmatched LOTUS biosource need not imply a missing key. This dataset-wide grouping can scan many hits even with `LIMIT 25`.

```sql
WITH per_file AS (
    SELECT r.dataset_id, r.filename, h.matched_short_inchikey AS short_inchikey,
           COUNT(*) AS hit_rows, COUNT(DISTINCT h.feature_id) AS features
    FROM annotation_current_runs AS r
    JOIN annotation_hits AS h ON h.run_id = r.id
    WHERE r.dataset_id = :dataset_id
      AND h.matched_short_inchikey IS NOT NULL
      AND h.matched_short_inchikey <> ''
    GROUP BY r.dataset_id, r.filename, h.matched_short_inchikey
)
SELECT short_inchikey, COUNT(*) AS files,
       SUM(features) AS file_feature_occurrences, SUM(hit_rows) AS hit_rows
FROM per_file
GROUP BY short_inchikey
ORDER BY hit_rows DESC, short_inchikey
LIMIT 25;
```

## Missing LOTUS matches and other errors

The latest attempt may be stale; `current_run_id`/`file_state` show whether a result is still eligible by MS2 hash. A missing registration has no current result. Keep the **raw errors**: the labels below are navigation hints, not an exhaustive or authoritative error taxonomy. `taxonomy_query` on an error attempt shows whether Hocofaisan tried the sample name or fell back to its resolved QID after a name lookup failed in LOTUS; if both lookups fail, the error includes the failed resolved-taxon lookup. Other common errors include absent/unreadable registered MS2 files, changed MS2 hashes, failed Cunaguaro taxon resolutions, and no accepted query/library spectra.

```sql
SELECT f.dataset_id, f.filename,
       CASE WHEN p.ms2_sha256 IS NULL THEN 'missing_registration'
            WHEN c.id IS NULL THEN 'no_current_annotation'
            ELSE c.status END AS file_state,
       c.id AS current_run_id, c.status AS current_status,
       c.error AS current_error,
       last.id AS latest_attempt_id, last.status AS latest_attempt_status,
       last.created_at, last.taxon_name, last.taxon_qid,
       last.taxonomy_query,
       CASE WHEN last.error LIKE '%not found in LOTUS%' THEN 'LOTUS lookup'
            WHEN last.error LIKE '%taxon resolution failed%' THEN 'taxon resolution'
            WHEN last.error LIKE '%no registered picoto MS2%' THEN 'missing registration'
            WHEN last.error LIKE '%registered MS2%' THEN 'registered MS2'
            ELSE 'other or no error' END AS latest_error_hint,
       last.error AS latest_error
FROM spectrum_files AS f
LEFT JOIN processed_spectra AS p
  ON p.dataset_id = f.dataset_id AND p.filename = f.filename
LEFT JOIN annotation_current_runs AS c
  ON c.dataset_id = f.dataset_id AND c.filename = f.filename
LEFT JOIN annotation_runs AS last
  ON last.id = (
      SELECT a.id FROM annotation_runs AS a
      WHERE a.dataset_id = f.dataset_id AND a.filename = f.filename
      ORDER BY a.id DESC LIMIT 1
  )
WHERE f.dataset_id = :dataset_id
  AND (c.status = 'error' OR last.status = 'error' OR p.ms2_sha256 IS NULL)
ORDER BY f.filename
LIMIT 100;
```

`no_hits` is a successful search with no candidate hits, **not** necessarily a LOTUS error. If a species filter returns nothing for a file with hits, inspect broader `matched_shared_rank` values and unsupported candidates rather than inferring the sample has no compounds.

Schema and selection semantics: [`hocofaisan/src/main.rs`](../hocofaisan/src/main.rs) (annotation schema, run selection, and recorded outcomes); [`spectral-matcher/src/search.rs`](../spectral-matcher/src/search.rs) and [`spectral-matcher/src/taxonomy.rs`](../spectral-matcher/src/taxonomy.rs) (scores and taxonomic reranking); [`peregrino/src/lib.rs`](src/lib.rs) (ingested file/registration keys).
