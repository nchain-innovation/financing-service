# Docker

Encapsulating the service in Docker removes the need to install the project dependencies on the host
machine. Only Docker is required to build and run the service.

The Compose file can be run from anywhere in the repository — the financing service image is built with the
**project root as its context**, and every path inside the Compose file resolves relative to this directory:

| Path | Purpose |
| --- | --- |
| [Dockerfile](Dockerfile) | Two-stage build: `rust:bookworm` builder, `debian:bookworm-slim` runtime |
| [docker-compose.yml](docker-compose.yml) | The financing service, plus the optional `woc` profile |
| [woc/](woc) | Config and build files for the `woc` profile's local WhatsOnChain stack |
| [scripts/](scripts) | Developer scripts for driving the regtest stack |
| [woc/docker-compose.wbl.yml](woc/docker-compose.wbl.yml) | Optional overlay: reach a wild-bit-lab node by container name |
| [.env.example](.env.example) | Template for the optional, gitignored `.env` |

## Docker Compose

To build the image and start the service in one step, run the following command in the project directory:

```bash
docker compose -f docker/docker-compose.yml up --build
```

This starts the financing service only. It publishes the REST API at http://localhost:8080, bind mounts
`data` into the container so config and dynamic client state persist on the host, and restarts the
container unless it is explicitly stopped. Set `FS_HTTP_PORT` to publish on a different host port; the
container port stays 8080.

Secrets referenced from the config as `env:VAR_NAME` (see [Configuration](../docs/Configuration.md)) can be
placed in an optional `docker/.env` file, which Compose loads automatically if present. See
[.env.example](.env.example) for the supported variables.

To stop the service:

```bash
docker compose -f docker/docker-compose.yml down
```

## WoC Profile

The `woc` profile stands up a **local WhatsOnChain API** alongside the financing service, ported from the
`woc-stack` reference. It is a [Compose profile](https://docs.docker.com/compose/how-tos/profiles/), so
nothing below starts unless the profile is named explicitly — `docker compose up` on its own still brings up
the financing service and nothing else.

```bash
docker compose -f docker/docker-compose.yml --profile woc up --build
```

The WoC API is published at http://localhost:5010. From inside the stack it is `http://woc-api:8084` — the
container port is fixed by the image, so `WOC_API_PORT` only moves the host side.

Starting the profile does not by itself point the financing service at it: set `network_type` and `url` in
`data/financing-service.toml` as well, per
[Pointing the service at the local WoC](#pointing-the-service-at-the-local-woc).

Note the routes carry **no `/v1/bsv/<network>` prefix**, unlike public WhatsOnChain — this build serves a
single network, so the paths are bare:

```bash
curl http://localhost:5010/chain/info
```

```json
{"chain":"regtest","blocks":0,"bestblockhash":"0f9188f1...2206","difficulty":4.65e-10,"pruned":false}
```

`/woc` answers `Whats On Chain` and makes a cheap liveness check. The image also listens on 8085 (a second,
Fiber-based server, `fiber_port` in its settings); 8084 is the one that serves these routes.

### What it starts

| Service | Role | Published |
| --- | --- | --- |
| `woc-api` | WhatsOnChain-compatible REST API | **5010** |
| `utxo-store` | UTXO index, backed by Scylla | — |
| `utxos-mempool` | Mempool UTXO view, backed by Aerospike | — |
| `chain-listener` | Subscribes to the node's ZMQ feed, publishes to RabbitMQ | — |
| `scylla` | Storage for `utxo-store` | — |
| `aerospikedb` | Storage for `utxos-mempool` | — |
| `rabbitmq` | Message bus between the listener and the stores | — |

Only `woc-api` publishes a host port; the rest talk to each other over the project network. State lives in
the named volumes `woc-scylla`, `woc-utxo-store`, and `woc-chain-listener`, so `docker compose --profile woc
down -v` resets the stack.

To stop it:

```bash
docker compose -f docker/docker-compose.yml --profile woc down
```

### It indexes a node, it does not run one

There is no bitcoind in this profile. `WOC_BSV_HOST` names the node to index, and defaults to
`host.docker.internal` — mapped to the host gateway — so a node running on the Docker host works with no
configuration. `WOC_BSV_PORT` (RPC, 18332), `WOC_BSV_ZMQ_PORT` (28332), and `WOC_BSV_PASSWORD` follow the
same pattern. See [.env.example](.env.example).

Until the node is reachable, the stack starts but indexes nothing. `rabbitmq`, `scylla`, and `aerospikedb`
go healthy as normal; `woc-api` serves on 5010 but has no chain data; `chain-listener` and `utxos-mempool`
log `could not dial ZMQ` and retry every 10s; and `utxo-store` exits on the first failed block fetch and is
restarted by Compose, so it sits in a restart loop. That is the expected state without a node, not a
misconfiguration.

## Local Development Against wild-bit-lab

Regtest nodes are set up and run manually from the
[wild-bit-lab](https://github.com/nchain-innovation/wild-bit-lab) repository — nothing in this repository
starts one.

### 1. Start the nodes

```bash
git clone git@github.com:nchain-innovation/wild-bit-lab.git
```

```bash
cd wild-bit-lab && docker compose --file one-node.yml up
```

Pick `one-node.yml`, `three-node.yml`, or `five-node.yml` depending on how many nodes you want. This also
brings up wild-bit-lab's orchestrator, block explorer, and dashboard. Wait for `node1` to report healthy.

### 2. Publish the financing service somewhere else

wild-bit-lab's block explorer occupies host 8080 whenever its stack is up, so move the REST API aside:

```bash
FS_HTTP_PORT=8081 docker compose -f docker/docker-compose.yml up --build
```

Or put `FS_HTTP_PORT=8081` in `docker/.env`.

### 3. Enable REST and ZMQ on the node

wild-bit-lab's stock `bitcoin.conf` is not sufficient for this stack, and neither shortfall announces
itself clearly:

| Needed by | Setting | Stock wild-bit-lab |
| --- | --- | --- |
| `utxo-store` (catch-up) | `rest=1` | **absent** — `/rest/*` returns 404 while JSON-RPC works fine |
| `chain-listener`, `utxos-mempool` | `zmqpub*` on 28332 | **absent** — nothing binds 28332 at all |

Add to wild-bit-lab's `data/bitcoin.conf` — note it is shared by every node via `connect=node1..node5`:

```conf
rest=1
zmqpubhashblock=tcp://0.0.0.0:28332
zmqpubrawblock=tcp://0.0.0.0:28332
zmqpubhashtx=tcp://0.0.0.0:28332
zmqpubrawtx=tcp://0.0.0.0:28332
```

Restart the nodes, then confirm REST answers `200`:

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://localhost:18332/rest/chaininfo.json
```

### 4. Point the service at the local WoC

The service does not infer this from the profile — edit `[blockchain_interface]` in
`data/financing-service.toml` before starting the stack, setting `network_type` to the node's network and
`url` to the woc-api the profile brings up:

```toml
[blockchain_interface]
interface_type = "woc"
network_type = "regtest"
url = "http://woc-api:8084"
```

`woc-api:8084` is the Compose service name and container port, which is how the financing service reaches it
across the project network; a host `cargo run` uses the published port, `http://localhost:5010`. Leave `url`
unset and the service talks to public WhatsOnChain instead — see
[Pointing the service at the local WoC](#pointing-the-service-at-the-local-woc). The startup log says which
one it chose.

### 5. Start the stack with the wild-bit-lab overlay

wild-bit-lab publishes node RPC on host 18332, but **not** ZMQ on 28332 — so the default
`WOC_BSV_HOST=host.docker.internal` cannot reach the ZMQ feed no matter how the node is configured.
[woc/docker-compose.wbl.yml](woc/docker-compose.wbl.yml) closes that by attaching the woc services to
wild-bit-lab's own network and addressing the node as `node1`:

```bash
docker compose -f docker/docker-compose.yml \
               -f docker/woc/docker-compose.wbl.yml \
               --profile woc up --build
```

Set `FS_HTTP_PORT=8081` in `docker/.env` first — wild-bit-lab's block explorer holds host 8080.

`regtest_network` is declared **external** in the overlay, with two consequences: if wild-bit-lab is not
running you get an immediate `network regtest_network ... could not be found` instead of a half-started
stack, and `down` here never disturbs wild-bit-lab, which owns the network. Override the name with
`WBL_NETWORK` if you have altered wild-bit-lab's compose files.

Once up, `chain-listener` logs `ZMQ: Connecting to tcp://node1:28332` followed by its subscriptions, and
`utxo-store` completes catch-up and settles on `starting blocks subscription` instead of restarting.

### Configuration lives in the TOML, not in .env

`docker/.env` is optional. Everything the service reads comes from `data/financing-service.toml`; the only
things that must be environment variables are those Docker itself consumes before a container exists, plus
secrets:

| Setting | Where it belongs | Why |
| --- | --- | --- |
| Node URL, network type, telemetry, rate limits | `data/financing-service.toml` | Read by the service at runtime |
| `FS_HTTP_PORT` | `docker/.env` | Host-side port publishing, resolved by the Docker engine |
| `WOC_API_PORT`, `WOC_BSV_*`, `WBL_NETWORK` | `docker/.env` | Consumed by Compose to wire up the `woc` profile |
| `FS_ADMIN_API_KEY`, `FS_CLIENT_*` | `docker/.env` or the shell | Kept out of the config file by design; the service warns on inlined secrets |

Compose cannot parse TOML, which is why the middle two cannot move. See [.env.example](.env.example).

### Pointing the service at the local WoC

Set `url` in `data/financing-service.toml`. That is what selects the self-hosted instance — leave it unset
and the service talks to public WhatsOnChain:

```toml
[blockchain_interface]
interface_type = "woc"
network_type = "regtest"
url = "http://woc-api:8084"      # in Docker: service name + container port
# url = "http://localhost:5010"  # host `cargo run`: published port
```

chain-gang's `WocInterface` cannot do this — it hard-codes `https://api.whatsonchain.com` and builds
`/v1/bsv/{network}/...` paths. A self-hosted woc-api serves one network and drops that prefix, and splits
balance across two endpoints, so [src/woc_interface.rs](../src/woc_interface.rs) implements that
dialect and [src/blockchain_factory.rs](../src/blockchain_factory.rs) picks between the two on `url`. The
startup log says which was chosen.

**The client WIF must match the network.** A mainnet WIF (`K...`/`L...`) yields a mainnet address, which a
regtest woc-api reports as `isvalid: false` and answers with HTTP 500 — showing up as
`update_balance - failed`. Use a regtest key (`c...`); `bitcoin-cli dumpprivkey <address>` on the node will
give you one.

Add a client and read its balance back through the service:

```bash
curl -X POST -H 'Content-Type: application/json' \
     -d '{"client_id":"regtest1","wif":"<regtest-wif>"}' \
     http://localhost:8081/client
```

```bash
curl http://localhost:8081/client/regtest1/balance
```

`{"confirmed":505000000000,"unconfirmed":0}` — served by the local stack, with no outbound call to
whatsonchain.com.

Live tests for the adapter run against a running profile:

```bash
WOC_TEST_ADDRESS=<funded-address> cargo test woc_local -- --ignored --nocapture
```

### Scripts

[scripts/topup.sh](scripts/topup.sh) funds a client on regtest, creating it with a freshly generated key if it does not
exist yet:

```bash
docker/scripts/topup.sh <client_id> [amount_in_bsv]
```

```
client 'freshclient' not found -- creating it
created client 'freshclient' at moyKHKSUPDsV4FzC5uAUJ5E6wohfiwBwXm
balance before: 0.00000000 BSV
sent 12 BSV in 0f8d2ca331e100ae21bd0ef7a12e0964ff3f4bedfecb0fc4db10e77112beff6b
mined 1 block to confirm
balance after:  12.00000000 BSV
topped up 'freshclient' by 12.00000000 BSV
```

It spends from the node's own wallet, mines a block to confirm, and waits for the WoC stack to index before
reporting the new balance. Mining is on demand on regtest, so nothing confirms until the script asks for it.

**Client keys are generated off-node, deliberately.** The obvious approach — `getnewaddress` followed by
`dumpprivkey` — leaves the key in the node's wallet, and coin selection will then spend that client's coins
to fund *any* later transaction. The balance silently drops, with no error anywhere: funding one client can
drain another. The script generates the WIF itself (base58check over `0xEF || key || 0x01`) so the node has
never seen it and cannot spend it. This is the one part that needs `python3`.

Clients created any other way may still be wallet-owned; the script checks with `validateaddress` and warns
when they are. It also locks the target client's own outputs during a top-up, so at minimum a top-up can
never be funded from the balance it is meant to increase.

Defaults are overridable by environment variable — `FS_URL` (default `http://localhost:8081`),
`NODE_CONTAINER` (`node1`), `NODE_RPC_PORT`/`NODE_RPC_USER`/`NODE_RPC_PASSWORD`, `CONFIRM_TIMEOUT`, and
`FS_ADMIN_API_KEY`/`FS_CLIENT_API_KEY` when the API is authenticated. Run it with no arguments for usage.

Clients accumulate in `data/dynamic.toml`; remove a test one with:

```bash
curl -X DELETE http://localhost:8081/client/freshclient
```

[scripts/mine.sh](scripts/mine.sh) mines blocks on demand:

```bash
docker/scripts/mine.sh [count]
```

```
mined 1 block to mr35xPEsquk3EL7Zv88k2p1iVSX8cDfsiF
height: 110 -> 111
tip:    769594dba372f0ac432dcedf34e2265b78941a740fab14aa811e72afe99552aa
woc:    111 (up to date)
```

Regtest produces no blocks by itself, so anything waiting on a confirmation waits forever until something
mines. `count` defaults to 1; pass 101 to mature a coinbase output so it becomes spendable. `MINE_ADDRESS`
sends the coinbase to a specific address, otherwise a new wallet address is used.

It returns immediately rather than waiting for the WoC stack, and reports the gap when it sees one —
`woc: 117 (still indexing, node is at 119)`. That lag is normal and clears in a few seconds; `topup.sh` does
wait, because it has to read the balance back.

Both scripts take the same node settings — `NODE_CONTAINER` (default `node1`), `NODE_RPC_PORT`,
`NODE_RPC_USER`, `NODE_RPC_PASSWORD`.

### Resetting after the chain is reset

The WoC stack's indexes are only valid for the chain they were built against. If wild-bit-lab's node is
recreated — not merely restarted — it comes back with a fresh chain and wallet, and the stack has to be
reset alongside it, or `utxo-store` will loop on a block the node no longer has:

```bash
docker compose -f docker/docker-compose.yml -f docker/woc/docker-compose.wbl.yml --profile woc down
```

```bash
docker volume rm financing-service_woc-scylla financing-service_woc-utxo-store financing-service_woc-chain-listener
```

```bash
docker compose -f docker/docker-compose.yml -f docker/woc/docker-compose.wbl.yml --profile woc up -d
```

Only derived data is discarded — the indexes rebuild from the node. Client keys in `data/dynamic.toml` are
untouched, but their **balances will be zero**: those coins existed on the old chain. Re-fund them with
[scripts/topup.sh](scripts/topup.sh).

Confirm the node's config survived — `bitcoin.conf` is a bind mount, so `rest=1` and the `zmqpub*` lines
persist across container recreation:

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://localhost:18332/rest/chaininfo.json
```

### Troubleshooting

| Symptom | Cause |
| --- | --- |
| `utxo-store` in a restart loop, logging `failed to get raw block rest: ERROR: code 404` | The node has `rest=1` disabled. It fetches the genesis block over REST during catch-up, treats the 404 as fatal, and exits; `restart: unless-stopped` loops it. JSON-RPC working is not enough — REST is a separate interface on the same port. |
| `utxo-store` exits once at startup with `failed to bootstrap reader ... dial tcp ...:9042: connect: connection refused` | Scylla was reported ready before its CQL transport was listening. Fixed by the `cqlsh`-based healthcheck in [docker-compose.yml](docker-compose.yml); `nodetool status \| grep UN` goes green seconds too early. |
| `utxo-store` in a restart loop, logging `failed to get block header: <hash>: unexpected response code 500: Block not found`, and `chain-listener` logging `reorg detected` | The regtest chain was reset out from under the stack — recreating wild-bit-lab's containers discards the node's blocks and wallet, which live in the container's writable layer. The stack's indexes still describe the old chain, so `utxo-store` resumes from a block the node has never seen and exits. Fix by wiping the derived state (see below). |
| `could not dial ZMQ ... connection refused` from `chain-listener` / `utxos-mempool` | No `zmqpub*` on the node, or 28332 not reachable. These retry every 10s rather than exiting. |
| `Bind for 0.0.0.0:8080 failed: port is already allocated` | wild-bit-lab's block explorer holds host 8080. Set `FS_HTTP_PORT=8081`. |
| `aerospikedb` exits with `/etc/aerospike/aerospike.conf: Read-only file system` | The config is mounted over the path the image's entrypoint regenerates. It belongs at `/run/secrets/aerospike.conf` via the Compose secret — see [docker-compose.yml](docker-compose.yml). |

## Build and Run Manually

Alternatively, build and run the image directly. To build the docker image associated with the service run
the following command in the project directory:

```bash
./build.sh
```

This builds the Docker image `financing-service-rust`. The image includes a health check against
`GET /health`.

To start the Docker container:

```bash
./run.sh
```

This will provide a REST API at http://localhost:8080

## Building and Publishing Docker Images

This project creates the Docker image `financing-service`, which is published to Docker Hub. This makes it
accessible for use by other projects and applications within the ecosystem.

To build and publish the image to Docker Hub, run the following command:

```bash
./multi-build.sh
```

**Requirements**

- **Docker Buildx:** The script requires Docker's Buildx extension to be set as the active builder. Ensure
  Buildx is properly installed and selected as the current Docker engine. For help, see
  [Docker Buildx](https://docs.docker.com/build/builders/)

- **Publishing Permissions:** Only members of the `rndprototyping` team within the `nChain` Docker Hub
  organisation are authorised to publish images with the appropriate tags. Ensure you are logged in with the
  necessary permissions before running the script, else this will fail.
