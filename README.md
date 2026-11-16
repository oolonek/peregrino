# peregrino

Peregrino keeps a searchable record of a dataset's sample metadata and raw mass-spectrometry file references. It also records which processed spectra Picoto produced, so later annotation results and a local knowledge graph can be traced back to their source files. It does not download datasets or read raw spectra.

The flock helps turn mass-spectrometry samples and measurements into traceable evidence for possible chemical matches, with an optional local knowledge-graph export. [Start with the flock overview](https://github.com/oolonek/flock-roadmap/blob/main/README.md) to see how the crates work together before following a technical guide.

The name refers to the basking shark (*[Cetorhinus maximus](https://www.catalogueoflife.org/data/taxon/T72Q)*; [Wikidata](https://www.wikidata.org/wiki/Q185260)), a plankton-filtering shark.

[![Basking shark swimming off the Atlantic coast](https://thumb.wikimedia.org/wikipedia/commons/thumb/3/39/Cetorhinus_maximus_atlantic.jpg/330px-Cetorhinus_maximus_atlantic.jpg)](https://commons.wikimedia.org/wiki/File:Cetorhinus_maximus_atlantic.jpg)
Photo by Green Fire Productions; color adjusted by Togabi; [CC BY 2.0](https://creativecommons.org/licenses/by/2.0/) ([Wikimedia Commons source](https://commons.wikimedia.org/wiki/File:Cetorhinus_maximus_atlantic.jpg)).

## In the flock

Sapayoa obtains a dataset; Machaca checks its metadata-to-file links; Peregrino stores those links and registers Picoto's processed spectra. Hocofaisan adds scored **candidate hits**, not confirmed chemical identifications. Peregrino can inspect those results and export a local evidence graph for querying.

## Start here

After the overview, use [Mato's job instructions](https://github.com/oolonek/mato/blob/master/TECHNICAL.md#run-a-job) for the usual end-to-end workflow. If you are working directly with Peregrino, follow the [ingest instructions](TECHNICAL.md#ingest-validated-dataset-metadata) after extracting a dataset and choosing an exact metadata taxon column.

## Learn more

The [technical guide](TECHNICAL.md) covers ingest, registration, inspection, annotation queries, KG export, validation and graph semantics. See [annotation query recipes](ANNOTATIONS.md) and the [QLever deployment guide](kg/QLEVER.md) for specialist workflows. Machine-specific paths, addresses and deployment notes belong in the ignored `LOCAL.md` guide, when present.

For the difference between a sampled **taxon** and a metadata **observation subject** (for example, a leaf), see the [lay explanation and proposed Wikidata mapping workflow](kg/OBSERVATION-SUBJECT.md). The mapping is not yet part of KG export.

*Naming note: For new species names, we take inspiration from [colonizer](https://crates.io/crates/colonizer)'s `inspire` mode, which suggests names from the Catalogue of Life, rather than inventing each crate name ourselves.*
