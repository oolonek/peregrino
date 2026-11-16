# Serve a Peregrino evidence graph with QLever

QLever indexes Peregrino's N-Quads export for repeated local SPARQL queries; it does not load Wikidata. This is a **portable procedure**, not a claim that deployment on another machine has been tested. A toy graph was previously exported, indexed, and served on one workstation with `qlever 0.5.34` and the pinned engine image in [Qleverfile](Qleverfile). The [upstream v0.6.0 research recipe](QLEVER-RESEARCH.md) is source-checked but **unexecuted**. Keep the indexing and serving engine image identical; an upgrade needs a new index and query checks. The top-250 graph passed separate SHACL validation, but its larger QLever deployment and resource requirements are not established by the toy run.

## Export and index a graph

Run from the flock directory (the parent of `peregrino/`) with the `qlever` CLI and Docker available. Supply the paths and the **actual** dataset ID from your processed SQLite database; `1` is not a universal ID. `WORK` must be a new, nonexistent directory under an existing parent on a filesystem with enough disk space. This prevents accidental reindexing over a live index. Export refuses to overwrite an existing graph.

```sh
ROOT="$PWD"
DB=/path/to/processed/flock.sqlite
WORK=/path/to/new/qlever-index
cargo run --manifest-path "$ROOT/peregrino/Cargo.toml" -- show datasets --db "$DB"
printf 'Dataset ID for the selected DOI: '
read -r DATASET_ID
mkdir "$WORK"  # fails if WORK exists; choose another path instead
cargo run --release --manifest-path "$ROOT/peregrino/Cargo.toml" -- kg export \
  --db "$DB" --dataset-id "$DATASET_ID" --out "$WORK/graph.nq"
( cd "$WORK" && qlever -q "$ROOT/peregrino/kg/Qleverfile" index )
```

[Qleverfile](Qleverfile) sets `FORMAT = nq`, index name `mato-toy` and API port 7043. Peregrino currently emits N-Quads with no fourth graph label, so ordinary triple patterns query QLever's **default graph**. No Wikidata dump or text index is required. Inspect `qlever -q "$ROOT/peregrino/kg/Qleverfile" index --show` if your CLI supports command preview before an index run. The pinned image digest records reproducible engine bytes, not proof that the image is already cached on your host. Keep the graph and its index together outside the source tree.

For an **already exported** top-250 graph named `graph.nq`, choose its existing directory yourself instead of re-exporting. From the flock directory:

```sh
ROOT="$PWD"
WORK=/path/to/existing/top-250-graph-directory
# Only if the directory has graph.nq and no existing mato-top-250 index:
( cd "$WORK" && qlever -q "$ROOT/peregrino/kg/Qleverfile-top-250" index )
```

[Qleverfile-top-250](Qleverfile-top-250) uses index name `mato-top-250` and API port 7045. Index only once in a directory without that index; use a new directory for a changed export. Larger graphs may require substantially more disk, memory and time. SHACL validation is independent of indexing; see [the export and validation guide](../TECHNICAL.md#wikidata-linked-local-knowledge-graph).

## Serve on localhost

After indexing finishes, **on the server**, set `WORK` to the indexed directory, check the chosen container name and port are unused, and start the server. The following toy command uses the same engine digest as Qleverfile. It creates a persistent container; it does not replace an existing one. The bind mount is read-only, and the host port is explicitly bound to loopback rather than all interfaces. Do not use `qlever start` here: the documented CLI Docker path publishes the port without an explicit host address.

```sh
WORK=/path/to/existing/qlever-index
# Only if the mato-qlever-toy container and port 7043 are free:
docker run --detach --restart unless-stopped --name mato-qlever-toy --init \
  --user "$(id -u):$(id -g)" \
  --publish 127.0.0.1:7043:7043 \
  --mount "type=bind,src=$WORK,dst=/index,readonly" --workdir /index \
  --entrypoint ServerMain \
  docker.io/adfreiburg/qlever@sha256:27d51763ca24bfe49a4f9e4108babae376f2dffef1313086b95bd262f06e6492 \
  -i mato-toy -p 7043 -m 2G -c 512M -e 128M -k 1000 -j 4 -s 30s
curl --fail-with-body --silent --show-error http://127.0.0.1:7043/ping
```

For a separately indexed top-250 directory, use a separate `WORK`, container name `mato-qlever-top-250`, `--publish 127.0.0.1:7045:7045`, `-i mato-top-250 -p 7045`, and the **same** digest. The 2 GB query memory and 30-second timeout are starting values, not measured requirements for that graph. If a container already exists, inspect it with `docker ps --filter name=mato-qlever-toy` and `docker logs mato-qlever-toy`; `docker start mato-qlever-toy` restarts a stopped one without reindexing. Verify which index its bind mount uses before assuming it reflects recent graph changes.

Docker versions **older than 28.0.0** can expose localhost-published ports to hosts on the same L2 segment: upgrade Docker or arrange additional network isolation for private metadata. See [Docker's port-publishing warning](https://docs.docker.com/engine/network/port-publishing/). Avoid `--publish 7043:7043`, `0.0.0.0` binds, `--no-access-check`, and public access without an explicit auth/TLS and data-rights review. This is a local SPARQL API without an access token.

## Query the indexed graph

On the server, from any directory after setting `ROOT` to the flock directory (the parent of `peregrino/`):

```sh
ROOT=/path/to/oolonek/isla-gabriola-flock
curl --fail-with-body --silent --show-error \
  --header 'Accept: text/tab-separated-values' \
  --data-urlencode "query@$ROOT/peregrino/kg/coverage.rq" \
  http://127.0.0.1:7043/
curl --fail-with-body --silent --show-error \
  --header 'Accept: text/tab-separated-values' \
  --data-urlencode "query@$ROOT/peregrino/kg/taxonomic-evidence.rq" \
  http://127.0.0.1:7043/
```

For the larger index, change the URL to port 7045 **only if** that server is running. `/ping` checks readiness. A request to `/` without a query is not a browser landing page; it can return “Unknown path”. These queries trace source taxa, file hashes, features, historical scoring runs, reference organisms and candidate chemical QIDs. A scored candidate is **not** verified compound presence. Wikidata IRIs link out; this index does not contain Wikidata statements unless you separately and deliberately import or federate them.

## Access from a laptop over SSH

The server and laptop must both permit SSH TCP forwarding; replace `user@server.example.org` with a reachable SSH account/hostname, and choose a free laptop port. Run on the **laptop** and keep the tunnel terminal open:

```sh
ssh -N -o ExitOnForwardFailure=yes \
  -L 127.0.0.1:17043:127.0.0.1:7043 \
  user@server.example.org
```

In another laptop terminal, `curl --fail-with-body --silent --show-error http://127.0.0.1:17043/ping` checks the forward. To query, send SPARQL to `http://127.0.0.1:17043/` with a suitable `Accept` header as above. The tunnel ends when SSH exits; changing the laptop port changes only the left side of `-L` and the URL. SSH tunneling from a separate laptop was **not exercised** in the original toy deployment; the example is a configuration pattern, not a verified connection to your server.

## Optional graphical QLever UI

The graphical UI is a **separate** service with its own SQLite database and UI port; it is not the SPARQL endpoint. [qlever-ui.yml](qlever-ui.yml) defines a toy backend whose browser-visible `baseUrl` is `http://127.0.0.1:7043`. Obtain a compatible QLever UI image by following [upstream's installation guide](https://github.com/qlever-dev/qlever-ui/blob/master/docs/install_qleverui.md) and review its [config import](https://github.com/qlever-dev/qlever-ui/blob/master/backend/management/commands/config.py). The original deployment used a locally cached image; its host-local image ID is not a portable image reference. The following Linux/Docker host-network pattern depends on the selected image containing `/app/db/qleverui.sqlite3`, `manage.py` and `qlever.wsgi:application`; inspect the image before using it. No fresh-host UI installation is claimed tested.

Choose an **empty** `UI_DATA` directory, set `UI_IMAGE` to the compatible image you obtained, and `ROOT` to the flock directory (the parent of `peregrino/`). Seeding is **only for a newly created UI directory**; never seed over an existing UI database or re-import with `--hide-all-other-backends` if it already has other backends.

```sh
ROOT=/path/to/oolonek/isla-gabriola-flock
UI_DATA=/path/to/new/qlever-ui-data
UI_IMAGE=qleverui:latest  # replace with your verified image reference
mkdir "$UI_DATA"  # fails if already present
docker run --rm --user "$(id -u):$(id -g)" \
  --mount "type=bind,src=$UI_DATA,dst=/data" \
  --entrypoint cp "$UI_IMAGE" /app/db/qleverui.sqlite3 /data/qleverui.sqlite3
docker run --rm --network host --user "$(id -u):$(id -g)" \
  --mount "type=bind,src=$UI_DATA,dst=/app/db" \
  --mount "type=bind,src=$ROOT/peregrino/kg/qlever-ui.yml,dst=/app/mato-ui.yml,readonly" \
  --env QLEVERUI_DATABASE_URL=sqlite:////app/db/qleverui.sqlite3 \
  --entrypoint python "$UI_IMAGE" \
  manage.py config default /app/mato-ui.yml --hide-all-other-backends
```

Only after reviewing credentials, available ports, host-network isolation and CSRF origins, start the UI with the **same** image and data directory (only if its container is absent):

```sh
docker run --detach --restart unless-stopped --name mato-qlever-ui --init \
  --network host --user "$(id -u):$(id -g)" \
  --mount "type=bind,src=$UI_DATA,dst=/app/db" \
  --env QLEVERUI_DATABASE_URL=sqlite:////app/db/qleverui.sqlite3 \
  --env QLEVERUI_ALLOWED_HOSTS=127.0.0.1,localhost \
  --env QLEVERUI_CSRF_TRUSTED_ORIGINS=http://127.0.0.1:17044,http://127.0.0.1:7044 \
  --entrypoint gunicorn "$UI_IMAGE" \
  --bind 127.0.0.1:7044 --workers 2 --limit-request-line 10000 \
  qlever.wsgi:application
```

`--network host` here is for Linux Docker and lets the UI reach the server's `127.0.0.1:7043`; Gunicorn still binds only server-loopback port 7044. A bundled UI database may contain a demo administrator account: **never expose this UI without changing credentials and reviewing Django security settings**. For a laptop browser, forwarding *only* 7044 is insufficient: its JavaScript must also reach its configured API at laptop `127.0.0.1:7043`. On the laptop, provided ports 17044 and 7043 are free:

```sh
ssh -N -o ExitOnForwardFailure=yes \
  -L 127.0.0.1:17044:127.0.0.1:7044 \
  -L 127.0.0.1:7043:127.0.0.1:7043 \
  user@server.example.org
```

Open `http://127.0.0.1:17044/default`, not just the UI root. To add a second backend on port 7045, configure it independently (the UI must point to `http://127.0.0.1:7045`) and **also** forward `-L 127.0.0.1:7045:127.0.0.1:7045`; the UI's backend list does not merge indexes. In the original UI, the sample-metadata example returned 3 toy records and the ranked-candidate example returned 20 candidate rows; **those counts are observations for that toy graph only**. “All candidate annotations with taxonomic score > X and spectral score > Y” uses strict thresholds; edit `VALUES (?X ?Y) { (5.0 0.7) }` to change them, and use *show all results* beyond the initial preview. `?sourceTaxonQid` identifies the sample, `?scoringTaxonQid` the run's scoring taxon, `?matchedOrganismQid` the selected LOTUS biosource, and `?chemicalQid` a **library candidate**. The selected supporting LOTUS structure can differ from that candidate: in the observed toy graph [Q105282298](https://www.wikidata.org/wiki/Q105282298) was boosted by short-key support from [Q105282296](https://www.wikidata.org/wiki/Q105282296), with [reference Q105282295](https://www.wikidata.org/wiki/Q105282295) / DOI `10.1007/BF00565881`. Scores do not prove metabolite presence or a `P703` claim. The longest-evidence-path example uses the real DOI predicate `wdt:P356` on the local dataset, but does not assert unexported Wikidata statements such as `P225`, `P235` or `P703`.

## Refresh, stop and data boundaries

`docker stop mato-qlever-toy` and `docker start mato-qlever-toy` stop/restart an **existing** toy container without deleting its index; inspect the container first and replace its name for other deployments. To refresh annotations after a matcher or LOTUS change, rerun Hocofaisan using your own library, LOTUS CSV, config and database paths before exporting; old successful runs can remain historical. Export a **new** graph to a **new** index directory and index it while the previous server stays up. After verifying the new index, deliberately stop and replace only the relevant container, using the new `WORK` mount, then compare [coverage.rq](coverage.rq) against the expected dataset. Do not use `qlever index --overwrite-existing` on the live index. Preserve the old index for rollback until the new one is checked. Change both `--publish` and `-p` if changing an API port, and align any UI `baseUrl`/laptop forwards.

An observed refreshed toy export on one workstation was about 58 MiB; QLever parsed 363,136 input statements and indexed 333,228 distinct statements in roughly two seconds **after export**. These are not sizing or performance guarantees. The index is writable while indexing and mounted read-only when served. The export includes verbatim original metadata JSON: review access restrictions, privacy, data licensing and sensitive fields **before** sharing a graph or exposing a service. QLever's [engine](https://github.com/ad-freiburg/qlever/blob/v0.6.0/LICENSE) and [CLI](https://github.com/qlever-dev/qlever-control/blob/v0.6.0/LICENSE) are Apache-2.0; this does not license source dataset metadata for redistribution. Retain source N-Quads and the image digest for reproducibility; consult [source-backed security and format research](QLEVER-RESEARCH.md) and [KG export limits](../TECHNICAL.md#wikidata-linked-local-knowledge-graph).
