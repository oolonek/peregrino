# QLever for the Peregrino toy N-Quads export

**Status:** source-checked recipe, **not run** here. The parent integration must smoke-test on the target host. These commands target one bounded, already exported `*.nq` file, not a hosted/public service.

## Findings and choices

- Peregrino serializes *triples* using an N-Quads serializer (`Graph::add` calls `serialize_triple`), so its present export has no graph label and its statements belong to the **default graph**; query with ordinary `{ ?s ?p ?o }`, not `GRAPH <...>`. N-Quads permits an optional fourth graph label for *named* graphs; QLever's `FORMAT = nq` must be explicit (default is `ttl`). Do not use `MULTI_INPUT_JSON`'s optional `graph` to move this export into a named graph. Sources: [local serializer](../src/kg.rs), [W3C N-Quads specification](https://www.w3.org/TR/n-quads/), [Qleverfile `FORMAT` and `MULTI_INPUT_JSON`](https://docs.qlever.dev/qleverfile/#section-data) ([index options](https://docs.qlever.dev/qleverfile/#section-index)).
- Prefer QLever's official `qlever` CLI to construct the index; its `INPUT_FILES` checks existence and `CAT_INPUT_FILES` supplies the input stream. The CLI supports Docker/Podman/native; default container image is `docker.io/adfreiburg/qlever`. Its own quickstart recommends `qlever index`, then `qlever start`, and documents `--show` for previewing its generated commands. Sources: [quickstart](https://docs.qlever.dev/quickstart/), [Qleverfile settings](https://docs.qlever.dev/qleverfile/), [pinned index implementation](https://github.com/qlever-dev/qlever-control/blob/v0.6.0/src/qlever/commands/index.py).
- **Do not use `qlever start` for this localhost-only deployment:** its Docker path publishes `-p PORT:PORT` without a host address, and `[server] HOST_NAME` affects only where the CLI connects, not the listening/binding address. Docker says such unqualified published ports are reachable on all host interfaces. Start the same official image manually with `-p 127.0.0.1:7043:7043`; Docker versions **older than 28.0.0** have a documented same-L2 leakage even with localhost publication, so upgrade Docker or use additional network isolation before trusting this for private data. Sources: [CLI startup implementation](https://github.com/qlever-dev/qlever-control/blob/v0.6.0/src/qlever/commands/start.py), [CLI container implementation](https://github.com/qlever-dev/qlever-control/blob/v0.6.0/src/qlever/containerize.py), [HOST_NAME semantics](https://docs.qlever.dev/qleverfile/#section-server), [Docker port-publishing guide](https://docs.docker.com/engine/network/port-publishing/).
- QLever engine and CLI are Apache-2.0, **not** a grant to distribute Peregrino's graph. The export contains original metadata JSON; review permissions/sensitive content before exposure. Without an access token, QLever disallows privileged updates; avoid `--no-access-check` and do not expose a token in the Qleverfile. Sources: [engine license](https://github.com/ad-freiburg/qlever/blob/v0.6.0/LICENSE), [CLI license](https://github.com/qlever-dev/qlever-control/blob/v0.6.0/LICENSE), [updates/access control](https://docs.qlever.dev/update/#configuration), [local export description](../TECHNICAL.md#wikidata-linked-local-knowledge-graph).

## Recommended exact recipe (unexecuted)

From the flock directory (the parent of `peregrino/`), with working Docker, `uvx`, and a *previously created* N-Quads export at `$GRAPH`, choose a **new, nonexistent** `$WORK` directory under an existing parent (do not point it at a live index):

```sh
GRAPH=/path/to/existing/graph.nq
WORK=/path/to/new/qlever-index
mkdir "$WORK"  # fails if the directory already exists
cp -- "$GRAPH" "$WORK/toy.nq"
cd "$WORK"
cat > Qleverfile <<'EOF'
[data]
NAME = mato-toy
FORMAT = nq

[index]
INPUT_FILES = toy.nq
CAT_INPUT_FILES = cat toy.nq
PARALLEL_PARSING = true
STXXL_MEMORY = 1G

[server]
PORT = 7043
HOST_NAME = localhost
MEMORY_FOR_QUERIES = 1G
CACHE_MAX_SIZE = 256M
CACHE_MAX_SIZE_SINGLE_ENTRY = 128M

[runtime]
SYSTEM = docker
IMAGE = docker.io/adfreiburg/qlever:0.6.0
EOF

uvx --from 'qlever==0.6.0' qlever index
# Explicit loopback binding: do NOT replace with qlever start.
docker run --detach --rm --name qlever.server.mato-toy --init \
  --user "$(id -u):$(id -g)" \
  --publish 127.0.0.1:7043:7043 \
  --mount "type=bind,src=$(pwd),target=/index" --workdir /index \
  --entrypoint bash docker.io/adfreiburg/qlever:0.6.0 \
  -c 'exec qlever-server -i mato-toy -j 2 -p 7043 -m 1G -c 256M -e 128M -k 100 -s 30s'

curl -fsS http://127.0.0.1:7043/ping
curl -fsS -H 'Accept: application/sparql-results+json' \
  --data-urlencode 'query=SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }' \
  http://127.0.0.1:7043/
```

This CLI/image pair is version-matched: [CLI `v0.6.0`/package version](https://github.com/qlever-dev/qlever-control/blob/v0.6.0/pyproject.toml), [engine `v0.6.0`](https://github.com/ad-freiburg/qlever/releases/tag/v0.6.0); the [official image-publishing workflow](https://github.com/ad-freiburg/qlever/blob/master/.github/workflows/docker-publish.yml) generates the `0.6.0` tag from `v0.6.0`. `uvx` usage is shown in [official quickstart](https://docs.qlever.dev/quickstart/#using-qlever). The Docker `--user`, mount, working directory and server flags mirror [the CLI's index container](https://github.com/qlever-dev/qlever-control/blob/v0.6.0/src/qlever/commands/index.py), [containerizer](https://github.com/qlever-dev/qlever-control/blob/v0.6.0/src/qlever/containerize.py), and [server command](https://github.com/qlever-dev/qlever-control/blob/v0.6.0/src/qlever/commands/start.py). `/ping` returns HTTP 200 while up ([monitoring](https://docs.qlever.dev/monitoring/)); the SPARQL HTTP endpoint is the configured `[server] PORT` ([settings](https://docs.qlever.dev/qleverfile/#section-server)). The `1G` indexing/query limits and `256M` cache are **suggested toy-size tuning, not measured requirements**: QLever's defaults are `5G` for indexing and querying and `2G` for cache; indexing's `STXXL_MEMORY` is an approximate upper bound and actual use can exceed it ([settings](https://docs.qlever.dev/qleverfile/#section-index), [server settings](https://docs.qlever.dev/qleverfile/#section-server)). Index creation also consumes disk for files alongside the input; keep `WORK` on a filesystem with space. No QLever UI is necessary.

### Stop and reindex (unexecuted)

From `$WORK`, `docker stop qlever.server.mato-toy` gracefully stops the server; `--rm` removes its container but **does not delete** bind-mounted files. For a changed graph, create and index a **new** `$WORK` directory using the recipe above, retaining the old index for rollback; after the new index succeeds, stop the old server and run the server command with the new directory mounted, choosing a nonconflicting container name. Do not use `--overwrite-existing` against a live index. Sources: [index command](https://github.com/qlever-dev/qlever-control/blob/v0.6.0/src/qlever/commands/index.py), [Docker bind mount semantics](https://docs.docker.com/engine/storage/bind-mounts/), [Docker container stop](https://docs.docker.com/reference/cli/docker/container/stop/). The fixed name/port in the example must be free before starting a replacement.

**Reproducibility limits:** `:0.6.0` is a published version tag but not immutable; for exact bytes, record and use the pulled `docker.io/adfreiburg/qlever@sha256:<digest>` in both Qleverfile and `docker run`, verifying that digest is the same architecture/engine intended for indexing and serving. `uvx --from qlever==0.6.0` pins the CLI version but not all transitive Python dependencies. No commands above, Docker version, package availability, index compatibility or graph counts have been verified on the target host. Sources: [Docker digest pinning](https://docs.docker.com/dhi/core-concepts/digests/), [CLI dependency specification](https://github.com/qlever-dev/qlever-control/blob/v0.6.0/pyproject.toml).
