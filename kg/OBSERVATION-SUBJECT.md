# Map observation subjects to Wikidata

## In plain language

The metadata may say that an observation concerns a **Leaf**. Wikidata has an item for the botanical concept [leaf (Q33971)](https://www.wikidata.org/wiki/Q33971). We can keep the word the data provider entered **and**, after checking its meaning, link that sample metadata record to the existing Wikidata item. The sampled organism's taxon is a *different* link: a record can concern a leaf of a plant without saying that the plant taxon is a leaf or that Wikidata contains an item for the particular physical sample.

This is a **curated vocabulary match**, not an automatic claim about chemical presence. The example metadata row supplied in the discussion has `"observation_subject":""`: there is nothing to map for that row. The existing top-250 database likewise has **250 empty observation-subject values**. In contrast, the full source metadata TSV has 3,425 rows, of which 1,544 have a nonempty value, including 1,207 `Leaf` values. Consequently the *existing* top-250 graph cannot answer a question about leaves, even though its SHACL validation passed. To query such links, we would first need records with reported subjects, reviewed term matches, a graph export that includes those matches, and a refreshed QLever index. None of that mapping/export behavior is implemented yet.

The first useful question would be “Which sample records report a leaf, and what analyses and ranked **candidate** chemical annotations are associated with their files?” A candidate is not proof that the chemical occurs in the leaf.

## Technical design (proposal; not the current graph)

### Source and scope

Peregrino stores every source column in `sample_rows.attributes_json`, including `observation_subject`. Today `kg export` emits that JSON as `kg:attributesJson` on a local `kg:SampleRecord`, but it does **not** emit a separate observation-subject literal or Wikidata link. Its `kg:reportedSourceTaxon` is a separate taxon-resolution result. A `SampleRecord` is an ingested metadata row, **not** a physical specimen; `kg:Acquisition` represents a raw-file analysis associated with the record. Do not infer an organ from the taxon, `sample_type`, or a blank `observation_subject`.

The complete source TSV at `MSV000102993/metadata/metadata/dbgi_massive_metadata_20260806.tsv` currently contains these exact nonempty, trimmed values (case retained):

| Source value | Rows | Proposed treatment |
| --- | ---: | --- |
| `Leaf` | 1,207 | Review botanical sense; candidate [Q33971](https://www.wikidata.org/wiki/Q33971) |
| `Stems` | 98 | Review; no QID assigned here |
| `Flower` | 68 | Review; no QID assigned here |
| `bud` | 56 | Review meaning; no QID assigned here |
| `Root` | 52 | Review; no QID assigned here |
| `bark` | 42 | Review meaning; no QID assigned here |
| `Fruit` | 18 | Review; no QID assigned here |
| `Seed` | 3 | Review; no QID assigned here |

The remaining 1,881 full-source rows are empty. These counts describe the **source TSV**, not the already-imported top-250 subset. A reproducible inventory of an ingested SQLite dataset is:

```sql
SELECT json_extract(attributes_json, '$.observation_subject') AS source_value,
       COUNT(*) AS rows
FROM sample_rows
WHERE dataset_id = :dataset_id
GROUP BY source_value
ORDER BY rows DESC, source_value;
```

### Review before linking

1. Inventory distinct **source** values for the selected dataset. Preserve the original cell exactly in `attributes_json`; use trimming/case folding only to propose candidates, never to silently replace the source.
2. Search Wikidata for each nonempty value and inspect the **item's meaning and biological scope**, not just its label. Confirm, for example, that Q33971 denotes a botanical leaf. Review singular/plural and language variants; words such as “bud” or “bark” may have several senses. Empty, ambiguous, composite, or unverified values remain unlinked. Record who reviewed a match, the source value, the chosen QID, the item URL, rationale, and mapping version/date. Keep this mapping local and dataset/field-scoped where meanings might vary. See the [primary-source research notes](OBSERVATION-SUBJECT-SOURCES.md) for Wikidata lookup mechanics.
3. Keep the reviewed mapping separate from original metadata and from the taxon/chemical lookup indexes. On export, emit a local source-text property for every nonempty source value and a Wikidata IRI **only for an approved unambiguous match**. Persist unresolved values and review decisions so they can be revisited; never interpret “not mapped” as “not a leaf.” Re-export and reindex to change an already-indexed graph.

For a row whose source text really is `Leaf` and whose match was approved, the proposed local RDF is:

```turtle
@prefix kg: <urn:mato:term:> .
@prefix wd: <http://www.wikidata.org/entity/> .

<urn:mato:local:example-sample-record> kg:observationSubjectText "Leaf" ;
                                       kg:reportedObservationSubject wd:Q33971 .
```

`kg:reportedObservationSubject` is a **proposed local predicate** from the metadata record to the Wikidata **item**. It is not a claim made *on Wikidata*; do not substitute `wdt:P31` (“instance of”), `wdt:P361` (“part of”), or the taxon's `kg:reportedSourceTaxon`: those assert different relationships. The example local subject URI above is illustrative, not an actual exported ID. A separate mapping provenance record/version is preferable to pretending that the WD item itself declares this particular sampling event. Extend [local SHACL shapes](shapes.ttl) only when the export actually emits these predicates; do not make an optional observation subject mandatory for every sample.

### Queries after implementation and reindexing

The following SPARQL is a **future query**, not a working query over the current top-250 graph. It follows the existing sample → acquisition → raw file → processing → feature → annotation-run → candidate path, plus the proposed observation-subject link. It uses the source-taxon QID independently of the leaf QID, and limits rows to scored candidates that have unambiguous chemical QIDs:

```sparql
PREFIX kg: <urn:mato:term:>
PREFIX prov: <http://www.w3.org/ns/prov#>
PREFIX wd: <http://www.wikidata.org/entity/>
PREFIX wdt: <http://www.wikidata.org/prop/direct/>
SELECT ?doi ?sampleId ?taxon ?feature ?chemical ?spectralScore
WHERE {
  ?dataset wdt:P356 ?doi ; kg:hasSampleRecord ?sample .
  ?sample kg:reportedObservationSubject wd:Q33971 ;
          kg:hasAcquisition ?acquisition .
  OPTIONAL { ?sample kg:sampleIdentifier ?sampleId . }
  OPTIONAL { ?sample kg:reportedSourceTaxon ?taxon . }
  ?acquisition kg:rawFile ?raw .
  ?processing prov:used ?raw ; kg:hasFeature ?feature .
  ?run kg:derivedFrom ?processing ; kg:hasCandidateHit ?hit .
  ?hit kg:feature ?feature ; kg:candidateChemical ?chemical ;
       kg:spectralScore ?spectralScore .
  FILTER(?spectralScore > 0.7)
} ORDER BY DESC(?spectralScore) LIMIT 100
```

The current top-250 graph carries only the original JSON and has no `kg:reportedObservationSubject` triples. The query therefore becomes meaningful **only after** implementing the reviewed mapping and rebuilding a graph that actually contains nonempty subjects. The `?run kg:derivedFrom ?processing` join intentionally excludes historical runs that cannot be connected to the current registered processing; `kg:candidateChemical` excludes unresolved chemical candidates. Taxa may remain unbound without dropping a scored hit.
