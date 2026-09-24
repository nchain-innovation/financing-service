# apigun

Load-test scripts for the Financing Service HTTP API, written for
[k6](https://k6.io/).

```
smoke.js        # does /fund work at all -- run this first
breakpoint.js   # at what request rate does it stop working
lib/
  config.js     # environment variables and defaults
  metrics.js    # the custom metrics
  fund.js       # the POST /fund request, its classification and recording
```

Both scripts share `lib/`, so their numbers are comparable.

## Running

```sh
# Correctness: 5 sequential fundings, strict thresholds.
k6 run tools/apigun/smoke.js

# Rate: hold 1, 2, 5, 10 and 20 req/s for 30s each.
k6 run tools/apigun/breakpoint.js
```

Defaults point at `http://127.0.0.1:9080` and the `event-rs` client from
`data/financing-service.toml`. Override with `-e`:

```sh
k6 run \
  -e BASE_URL=http://127.0.0.1:9080 \
  -e CLIENT_ID=event-rs \
  -e API_KEY=secret \
  -e RATES=1,5,10,25,50 \
  -e STAGE_DURATION=1m \
  tools/apigun/breakpoint.js
```

## Reading a breakpoint run

`breakpoint.js` declares one threshold pair per rate, so the summary comes out
as a table:

```
fund_ok{rate:01} ................: rate=1.00   ✓ rate>0.95
fund_ok{rate:02} ................: rate=1.00   ✓ rate>0.95
fund_ok{rate:05} ................: rate=1.00   ✓ rate>0.95
fund_ok{rate:10} ................: rate=0.81   ✗ rate>0.95
fund_ok{rate:20} ................: rate=0.34   ✗ rate>0.95
```

Rate tags are zero-padded to the width of the largest rate. k6 sorts
sub-metrics lexicographically with no hook to override it, so an unpadded tag
would list as 1, 10, 2, 20, 5; padding makes the string order match the numeric
order and the table read top to bottom.

**The highest rate whose `fund_ok` threshold still passes is the answer.** The
overall run is expected to report failure — it is meant to push past the
breaking point.

Then read `fund_outcome` to learn *why* it broke, and check
`fund_duration{rate:N}`: latency usually degrades a stage before the success
rate does.

## Finding max TPS

Max TPS here means the highest request rate at which `/fund` keeps answering
successfully. The per-rate thresholds are built for exactly this, so it is one
or two runs, not a manual climb:

```sh
# 1. Bracket it. A geometric ladder finds the order of magnitude in one run.
k6 run -e RATES=1,2,5,10,20,50,100 -e SUCCESS_THRESHOLD=1.0 tools/apigun/breakpoint.js

# 2. Refine around the knee. Say 20 passed and 50 failed:
k6 run -e RATES=20,25,30,35,40,45 -e STAGE_DURATION=1m -e SUCCESS_THRESHOLD=1.0   tools/apigun/breakpoint.js
```

The highest rate whose `fund_ok{rate:NN}` threshold passes is the answer. Each
stage is scored only on its own requests, so one run gives the whole curve.

Longer stages give a more trustworthy number: a 30s stage at 2 req/s is 60
requests, and a single failure is 1.7%. Use `STAGE_DURATION=1m` or more once
you are refining.

### Check these before believing the number

**Did k6 actually send the rate?** If it warns that it could not keep up, or
`vus_max` is pegged, the stage never reached its target and its label is
fiction. Raise `MAX_VUS`.

**What broke?** A drop in `fund_ok` is only a service limit if the requests
moved to `fund_failed` (5xx). The `fund_outcome` block in the summary is the
per-code breakdown that tells you, counted by the service's own `code` field:

```
fund_ok ..............................: 48.50% 65 out of 134
  { rate:04 }.........................: 75.00% 12 out of 16
  { rate:08 }.........................: 46.87% 15 out of 32
  { rate:16 }.........................: 43.75% 28 out of 64
fund_outcome .........................: 134    8.903259/s
  { code:broadcast_failed }...........: 65     4.318745/s
  { code:broadcast_rejected }.........: 4      0.265769/s
  { code:no_suitable_utxo }...........: 0      0/s
  { code:ok }.........................: 65     4.318745/s
  { code:rate_limited }...............: 0      0/s
  ...
```

Read it as: `fund_ok{rate:NN}` says *where* it broke, `fund_outcome{code:...}`
says *why*. Here the failures are `broadcast_failed`, a 5xx -- so this is a
genuine service or upstream limit, and 4 req/s is the last rate that held.

| Dominant code | What you actually found |
| --- | --- |
| `broadcast_failed`, `chain_unavailable`, `internal` (5xx) | The service or its upstream. **This is max TPS.** |
| `broadcast_rejected` | Concurrent requests for one client picked the same UTXO. Fan out across clients. |
| `no_suitable_utxo`, `insufficient_balance` | The wallet ran dry. Pre-split it and re-run. |
| `rate_limited` | `[web_interface.rate_limit]`, a deliberate answer. That *is* your configured ceiling. |

Every code is listed even when it never occurred, which is the point: a `0`
next to `rate_limited` means rate limiting was not in play, rather than leaving
you to wonder. k6 prints a sub-metric only when a threshold is declared for it,
so `lib/metrics.js` declares an always-passing `count>=0` for each code purely
to make the rows appear -- they are not assertions and never fail a run.

**Is the upstream the real limit?** On `woc` the chain-read pacing (3 req/s by
default) caps things well below anything the service imposes. For a
service-level number use a local regtest node and keep chain refreshes out of
the request path.

## Analysing failures from the service log

k6 tells you *that* requests failed and which `code` came back. The reason
behind the code, and whether the transactions the service reported as funded
actually reached the chain, are only in the financing-service's own log.

Set this up **before** the run. Logs go to stdout/stderr via
`tracing_subscriber::fmt` -- there is no log file -- and `run.sh` uses
`docker run -it --rm`, so the output is gone once the container stops:

```sh
# Docker: run detached, then follow into a file
docker logs -f <container> > fs.log 2>&1

# Or directly
cargo run 2>&1 | tee fs.log
```

**A release build logs only `warn` and above.**

`[logging] level = "info"` is enough for both techniques below. At a few
hundred failures a second this file grows quickly, so send it to disk rather
than a terminal.

### 1. Were there any double spends?

A conflict is logged with the transaction that got there first, so one grep
counts them:

```sh
grep -i "conflicted with" fs.log
```

Any hit is worth looking at: it is financing-service racing against itself,
two requests having planned against the same UTXO. Other non-retryable
refusals, where the upstream declined on its own terms, match `rejected:`
without naming a conflict.

### 2. Did the transactions actually land on chain?

A `200` means the upstream accepted the transaction, not that it confirmed. If
conflicting transactions were accepted optimistically, only one of them ever
confirms while the service reported both as funded.

**In a single-chain setup -- one top-up UTXO -- checking the last transaction
is enough.** Each funding transaction spends the change of the one before it,
so they form one chain, and a transaction cannot confirm unless its inputs
exist. If the last one confirmed, every ancestor confirmed with it.

Take the txid from the service log. Every attempt is logged as
`broadcasting funding tx <txid>`, so the last one is the last transaction the
service tried to broadcast.

Look that txid up on a block explorer, or `getrawtransaction` on regtest.

#### A stronger check: count, do not sample

Checking the last transaction cannot spot a missing one. Counting can, because
every funding transaction touches the client's address.

1. Get the address: `GET /client/{client_id}/address`.
2. Count its transactions **before** the run.
3. Run k6 and note the `fund_ok` count.
4. Wait for a block -- confirmed, not just in the mempool.
5. Count again: the increase should equal `fund_ok`.

A smaller increase is the gap -- fundings the service reported that never
reached a block.

## Environment variables

| Variable | Default | Used by | Meaning |
| --- | --- | --- | --- |
| `BASE_URL` | `http://127.0.0.1:9080` | both | Service base URL. |
| `CLIENT_ID` | `event-rs` | both | `client_id` in the request body. |
| `API_KEY` | *(none)* | both | Sent as `X-API-Key`; omit when the client has no key. |
| `SATOSHI` | `100` | both | Satoshi per outpoint. |
| `NO_OF_OUTPOINTS` | `1` | both | Outpoints per request. |
| `MULTIPLE_TX` | `false` | both | One transaction per outpoint instead of one carrying all. |
| `LOCKING_SCRIPT` | a throwaway P2PKH | both | Hex locking script; see [docs/LockingScripts.md](../../docs/LockingScripts.md). |
| `IDEMPOTENCY` | `off` | both | `unique` for a fresh `idempotency_key` per iteration, `replay` to reuse one key for the run. |
| `TIMEOUT` | `30s` | both | Per-request timeout. |
| `ITERATIONS` | `5` | smoke | Number of fundings. |
| `SLEEP` | `1` | smoke | Seconds between iterations. |
| `RATES` | `1,2,5,10,20` | breakpoint | Request rates (req/s) to hold, in order. |
| `STAGE_DURATION` | `30s` | breakpoint | How long each rate is held. |
| `RAMP_DURATION` | `5s` | breakpoint | Ramp between rates; these requests are excluded from the per-rate numbers. |
| `LATENCY_BUDGET` | `3000` | breakpoint | p95 milliseconds the service is expected to hold. |
| `SUCCESS_THRESHOLD` | `0.95` | breakpoint | Success rate a stage must hold to count as sustained. `1.0` means every request. |
| `PRE_ALLOCATED_VUS` / `MAX_VUS` | derived from the top rate | breakpoint | VU pool. Raise if k6 warns it cannot reach the arrival rate. |

## How outcomes are classified

k6's built-in `http_req_failed` counts every non-2xx as a failure, which is the
wrong lens here: the service answers a refusal it *expects* with `409` and a
machine-readable `code`. `lib/fund.js` classifies against
`ErrorCode::status` in [src/responses.rs](../../src/responses.rs) instead:

| Status | Metric | Meaning |
| --- | --- | --- |
| 200 | `fund_ok` | Funded. `fund_replayed` counts those that were idempotency replays. |
| 409 | `fund_refused` | Well-formed but conflicts with state — usually `no_suitable_utxo` or `insufficient_balance`. **The wallet, not the service.** |
| 429 | `fund_throttled` | `[web_interface.rate_limit]` turned the request away. |
| 422 | `fund_partial` | Some transactions broadcast and some did not. Needs a human. |
| 5xx, timeout | `fund_failed` | The service or its upstream broke. **This is the breakpoint signal.** |
| 400/401/404 | `fund_misconfigured` | The test itself is wrong. Aborts the run. |

Every outcome is also counted on `fund_outcome`, tagged with the service's
code: `fund_outcome{code:no_suitable_utxo}`.

## Caveats

**These scripts spend money.** `POST /fund` is a write path — every successful
iteration broadcasts a transaction. Run against regtest or a testnet client you
are willing to drain, and keep `SATOSHI` small.

**The wallet is usually the limit, not the service.** Once the client's UTXOs
are consumed, `/fund` answers `409 no_suitable_utxo` and the run is measuring
the wallet's shape rather than the service's throughput. Watch `fund_refused`:
if it climbs before `fund_failed` does, the number you got is the wallet's
limit. Pre-split the wallet into many outputs before a serious run.

**Per-rate thresholds are scored in isolation; global ones are not.** Each
`fund_ok{rate:NN}` sub-metric only collects samples carrying that tag, so a
stage is judged on its own requests and earlier stages cannot dilute it. The
*untagged* metrics (`fund_ok`, `fund_failed`) are whole-run aggregates with no
sliding window, so read those as a summary of the run, never as the cliff.

**Broadcast latency is upstream.** Most of `fund_duration` is the node or
mapi-lite answering, not the service. A breakpoint found against a slow
testnet upstream is that upstream's limit. For a service-level number, point
`[blockchain_interface]` at a local regtest node.
