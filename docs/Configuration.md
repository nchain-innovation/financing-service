# Financing Service - Configuration

Configuration for this service can be found in `data/financing-service.toml`. The file is read when the service starts.

Alternatively, set the `FS_CONFIG` environment variable to a JSON-encoded config object.

The file is composed of the following sections:

## [blockchain_interface]

Configures the blockchain interface. Supported `interface_type` values: `woc`, `uaas`, `rpc`, `test`.

```toml
[blockchain_interface]
interface_type = "woc"
network_type = "testnet"
# url = "http://localhost:5010"  # required for uaas, and for rpc (host:port)
# rpc_user = "rpcuser"                    # required for rpc
# rpc_password = "env:FS_RPC_PASSWORD"    # required for rpc
# rpc_import_addresses = true             # rpc only; default true
# max_requests_per_second = 3             # default 3 for woc, unset otherwise
```

Supported `network_type` values: `mainnet`, `testnet`, `stn`, `regtest`.

### Which interface reaches which network

The interface you choose constrains which networks you can reach, and this is usually the deciding factor for a local deployment.

| `interface_type` | Reaches | mainnet | testnet | stn | regtest |
|---|---|---|---|---|---|
| `woc` | WhatsOnChain, a public API | ✅ | ✅ | ✅ | ❌ |
| `uaas` | a UTXO as a Service instance (set `url`) | ✅ | ✅ | ✅ | ❌ |
| `rpc` | a node's JSON-RPC endpoint, directly | ✅ | ✅ | ✅ | ✅ |
| `test` | nothing — an in-process stub | — | — | — | — |

**`rpc` is the only interface that reaches regtest**, because no public explorer indexes a private chain. It also works against a node you control on any other network, which removes the dependency on a third-party API being up.

The interface serves both chain *reads* (balances, UTXOs) and, by default, the *broadcast* of funding transactions. The broadcast alone can be moved to a mapi-lite server with the optional [`[mapi_lite]`](#mapi_lite) section; reads stay here.

### The `rpc` interface

```toml
[blockchain_interface]
interface_type = "rpc"
network_type = "regtest"
url = "127.0.0.1:18443"
rpc_user = "rpcuser"
rpc_password = "env:FS_RPC_PASSWORD"
```

* `url` — the node's JSON-RPC host and port. A bare `host:port` is treated as `http://`; give a full URL for `https`.
* `rpc_user` / `rpc_password` — the node's RPC credentials, sent as HTTP basic auth on every call. **Point this only at a node you control**, since the credentials go to whatever host is configured.

`rpc_password` takes an `env:VAR_NAME` reference and is overridden by `FS_RPC_PASSWORD`; `rpc_user` behaves the same way with `FS_RPC_USER`. A plaintext `rpc_password` is reported at startup like any other plaintext secret.

#### Address imports

A node answers balance and UTXO queries from its own wallet, so an address it does not track reads as **zero, with no error**. The service therefore asks the node to watch each client's funding address — at startup for configured clients, and again whenever one is added through `POST /client`, so a new client works without a restart.

The import uses `importaddress` with the rescan disabled, because a freshly derived client key has no history to find and a rescan blocks the node's RPC connection while it runs.

```toml
rpc_import_addresses = false   # default: true
```

Turn it off if you manage the node's wallet yourself, or if it is a **descriptor wallet**, where `importaddress` is refused. With imports off, make sure each client's address is already tracked; `GET /client/{client_id}/address` gives you the address.

An import that fails is logged as a warning and does not stop the service — the node may be refusing for a reason you already know about. The warning names the address and says what follows from it: that balance reads zero and funding is refused until the node tracks the address. Worth watching for on first run, because a reachable node reporting an empty wallet otherwise looks like a bug at the caller.

#### Outbound rate limiting

The service reads chain state often: twice per client per refresh — once for the balance, once for the UTXOs — from the periodic timer, from `GET /balance`, and from `POST /fund` both before building a transaction and again on the error path. Several clients, or one client and a little traffic, and those bursts overlap.

**Those are two *calls*, which are no longer two requests.** Since chain-gang 0.11.3 (CS-456) a balance read is two HTTP requests and a UTXO read is one per 1000 UTXOs — 21 for the busy mainnet address that change was measured against. See the limit's scope below.

WhatsOnChain documents **"up to 3 requests/sec is free"** and answers `429 Too Many Requests` above it. A failed refresh is the mild consequence; sustained violation risks a ban, which no amount of retrying recovers from. So outbound calls are spaced:

```toml
max_requests_per_second = 3   # woc default; unset for rpc and uaas
```

Left unset it follows the interface. `woc` talks to a public API and is limited to 3/s. `rpc` and `uaas` are the operator's own servers, so they are not limited at all.

Set it explicitly to override either way — raise it if you have a paid WhatsOnChain plan, lower it for a node you want to go easy on, or set it to `0` to turn the limit off entirely.

The limit applies to the whole service, not per client: concurrent refreshes share one allowance, which is what a per-IP limit at the far end actually measures. At startup the chosen limit is logged at INFO.

**What it paces: calls, not HTTP requests (CS-457).** The limiter decorates the blockchain interface, so it takes one slot per `get_balance` or `get_utxo`, and chain-gang then issues however many requests that call needs inside it. For an address under 1000 UTXOs that is still one request per call and the guarantee holds exactly. Beyond it, this number is a floor on the spacing between calls rather than a ceiling on requests per second.

It cannot currently be enforced at the request level: chain-gang's WhatsOnChain client builds its own HTTP client internally and exposes no way to supply one, so those requests are unreachable from this service. CS-457 tracks it. If you point this at a busy address on a free WhatsOnChain plan, lower `max_requests_per_second` to leave headroom rather than assuming the configured number is what reaches the API.

#### When the interface is unreachable

A failed refresh is logged as a warning with the length of the run so far:

```
WARN blockchain interface failure #7: get_utxo failed: ... 429 Too Many Requests
```

and when it comes back, that is logged too:

```
INFO blockchain interface recovered after 7 consecutive failure(s)
```

The recovery line matters as much as the warnings. Without it, warnings simply stop, and a service that has recovered looks exactly like one that has given up. `GET /status` reports `blockchain_status` and `blockchain_update_time` alongside, for the same question asked at a point in time rather than in the log.

Note that `GET /health` stays `ok` throughout: it is a liveness check and deliberately depends on no upstream. `GET /ready` is the endpoint that reflects whether the service can do its job.

**`test` is a fixture, not a backend.** It is an in-process stub used by the unit tests, with a UTXO set injected directly by the test harness. It has no network of its own, so the `network_type` you set alongside it only affects address encoding. It will start and serve requests as a configured backend, but its UTXO set is empty, so balances read zero and funding is refused — useful for exercising the API surface, not for funding anything.

### A note on the default port

`[web_interface] port` defaults to `8080`, which several other nChain services also default to — mapi-lite among them. If you are running this alongside one of those, change the port here or there; nothing will respond on a port that another process already holds.

## [web_interface]

Configures the REST API endpoint for the service.

```toml
[web_interface]
address = "127.0.0.1"
port = 8080
# admin_api_key = "your-admin-secret"
```

When `APP_ENV=docker`, the service listens on `0.0.0.0` regardless of `address`.

Bind to `127.0.0.1` or place the service behind a reverse proxy on a private network when exposing funding endpoints.

### Admin authentication

Set `admin_api_key` to require a shared secret on `POST /client`. Clients must send either:

* `Authorization: Bearer <admin_api_key>`
* `X-API-Key: <admin_api_key>`

When `admin_api_key` is omitted, `POST /client` is unauthenticated. The service logs a warning at startup when the admin key is not configured.

### Rate limiting

Optional per-IP rate limiting is configured under `[web_interface.rate_limit]`:

```toml
[web_interface.rate_limit]
enabled = true
requests_per_second = 10
burst_size = 20
```

* `enabled` — turn rate limiting on or off (default: `false`)
* `requests_per_second` — sustained request rate allowed per client IP
* `burst_size` — maximum burst before limiting (defaults to `requests_per_second`)

When enabled, excess requests receive HTTP 429 with a JSON error body. `/health` and `/ready` are exempt so container orchestration probes are not throttled.

Behind a reverse proxy, the limit applies to the proxy's IP unless you configure the proxy to pass the original client address and implement a custom key extractor.

## [logging]

Configures the log level for the service.

```toml
[logging]
level = "info"
```

The logging level can be one of:

* `error` — very serious errors
* `warn` or `warning` — hazardous situations
* `info` or `information` — useful information
* `debug` — detailed information
* `trace` — very verbose information

## [telemetry]

Optional OpenTelemetry trace export via OTLP (gRPC). Disabled by default.

```toml
[telemetry]
enabled = true
service_name = "financing-service"
otlp_endpoint = "http://localhost:4317"
```

* `enabled` — export traces to an OTLP collector (default: `false`)
* `service_name` — `service.name` resource attribute (default: `financing-service`)
* `otlp_endpoint` — OTLP gRPC endpoint (default: `http://localhost:4317`)

When enabled, the service:

* Creates a span per HTTP request via `tracing-actix-web` (OpenTelemetry semantic conventions)
* Exports traces in batches to the configured collector
* Bridges existing `log` crate output through `tracing` to include `trace_id` in logs

Standard OpenTelemetry environment variables are also supported:

| Variable | Purpose |
|----------|---------|
| `OTEL_TRACES_EXPORTER=otlp` | Enable export when `telemetry.enabled` is `false` |
| `OTEL_SERVICE_NAME` | Overrides `telemetry.service_name` |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | Overrides `telemetry.otlp_endpoint` |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | Trace-specific OTLP endpoint |

Example with the OpenTelemetry Collector:

```bash
docker run -p 4317:4317 otel/opentelemetry-collector:latest
```

Then enable telemetry in config or set `OTEL_TRACES_EXPORTER=otlp` and `OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317`.

## [service]

```toml
[service]
utxo_refresh_period = 60
# chain_state_max_age_seconds = 60   # defaults to utxo_refresh_period
```

* `utxo_refresh_period` — seconds between the periodic refresh of every client's chain state.
* `chain_state_max_age_seconds` — how old that cached state may be before a request refreshes it again. Defaults to `utxo_refresh_period`.

### How often the service reads the chain

Every chain-state refresh is one request to the blockchain interface, and `POST /fund` and `GET /balance` each want current state. Refreshing on every request means the cost scales with traffic, which is what runs into a public API's rate limit — see [Outbound rate limiting](#outbound-rate-limiting).

So a request reuses the cached state while it is younger than `chain_state_max_age_seconds`, and the periodic refresh skips clients a request has already refreshed inside that window. At the default the two are the same length, so the steady-state cost is **one request per client per period however much traffic arrives**.

**The trade is staleness.** Widening the window widens the period in which the service can build a funding transaction against a view of the chain that has moved on. It is narrower than it looks: the service applies its own spends to the cache as it makes them, so this only matters if something *outside* the service spends from the same client key — another service sharing the WIF, or an operator moving funds by hand. If that is your deployment, lower it. `0` refreshes on every request, which is what the service did before this existed.

There is a backstop either way. A broadcast the upstream refuses — which is how a conflicting input shows up — marks the cached state stale, so the attempt after it works from a fresh read. Building on a stale view therefore costs one refused transaction, not a run of them.

## [idempotency]

Optional. Controls how long `POST /fund` idempotency records are retained; see [Idempotency](SupportedEndpoints.md#idempotency) for what they do. Both fields have defaults, so the section can be omitted entirely.

```toml
[idempotency]
ttl_seconds = 600
max_entries = 10000
```

* `ttl_seconds` — how long a completed record stays replayable. Should comfortably exceed your clients' retry window; too short and a legitimate retry funds a second time. Default `600`.
* `max_entries` — upper bound on retained records, so a client sending a fresh `idempotency_key` on every request cannot grow the store without limit. When full, the oldest record is dropped. Default `10000`.

Records are held **in memory only** and are lost when the service restarts, so a retry that spans a restart can still produce a second funding transaction.

## [fees]

Optional. Sets the fee the service pays on the funding transactions it builds. Both fields have defaults, so the section can be omitted.

```toml
[fees]
satoshis_per_kb = 100
use_mapi_fee_quote = true
```

* `satoshis_per_kb` — the rate, in satoshis per kilobyte. The fee for a transaction is `ceil(tx_bytes * satoshis_per_kb / 1000)`, rounded **up** so that rounding never underpays. Must be greater than zero; the service refuses to start otherwise, because a transaction paying no fee is not relayed. Default `100`.
* `use_mapi_fee_quote` — when `[mapi_lite]` is configured, take the rate from its `feeQuote` rather than from `satoshis_per_kb`. Default `true`. Without `[mapi_lite]` it has no effect, because there is nothing to ask.

### Upgrading from 4.2.0 or earlier pays a lower fee

Before this section existed the fee was hardcoded as `((tx_bytes / 1000) * 500) + 750`. That is a step function rather than a rate: 750 satoshi for any transaction under a kilobyte, jumping by 500 at each kilobyte after.

An ordinary funding transaction — one input, two outputs — is 217 bytes, so it used to pay **750** satoshi, an effective 3000 sat/KB. At the new default of 100 sat/KB it pays **22**.

That is the point of the change, but it is a change in behaviour and not only a refactor: **an existing deployment that upgrades without adding a `[fees]` section will pay less than it used to.** Set `satoshis_per_kb` explicitly if you need the old figures, and note that a rate your miners will not accept is only discovered at broadcast.

The lower fee also changes which UTXOs get spent. A cheaper transaction can be funded by a smaller input, so the service now reaches for small UTXOs where it previously had to break up a large one.

### Taking the rate from mapi-lite

With `[mapi_lite]` configured and `use_mapi_fee_quote` left at `true`, the service reads the **standard** mining fee from mapi-lite's `feeQuote` and converts it to satoshis per kilobyte, rounding up. One server then sets the rate for every service broadcasting through it, rather than each keeping its own number.

`satoshis_per_kb` remains the fallback. It is used before the first quote arrives, and whenever a refresh fails — a quote that cannot be fetched, names no fee, or gives a zero rate leaves the rate in force untouched. A mapi-lite that is down therefore stops the rate *changing* rather than stopping funding.

The quote is refreshed on the same sweep that refreshes balances (`service.utxo_refresh_period`), not on the funding path, so costing a transaction never waits on a request to mapi-lite. The rate can therefore be up to one sweep out of date.

## [mapi_lite]

Optional. When this section is present, funding transactions are **broadcast through a [mapi-lite](https://github.com/nchain-innovation/mapi-lite) server** instead of through the `[blockchain_interface]`. Chain reads — balances and UTXO refreshes — are unaffected and keep using `[blockchain_interface]`. Leave the section out and the service behaves exactly as before: transactions are broadcast via WhatsOnChain (or whichever interface is configured).

```toml
[mapi_lite]
base_url = "http://127.0.0.1:8080"
auth_token = "env:FS_MAPI_LITE_AUTH_TOKEN"
# timeout_seconds = 13
# health_timeout_seconds = 2
# max_retries = 2
# total_timeout_seconds = 45
```

* `base_url` — **required.** Base URL of the mapi-lite server, `http://` or `https://`. Surrounding whitespace and a trailing `/` are stripped before the URL is validated or used, so the value that is checked at startup is the one that is requested.
* `auth_token` — optional. Sent **verbatim** as the `Authorization` header on every request, so include the scheme the server expects, e.g. `"Bearer <secret>"`. Takes an `env:VAR_NAME` reference and is overridden by `FS_MAPI_LITE_AUTH_TOKEN`. A plaintext value is reported at startup like any other plaintext secret. Never logged.
* `timeout_seconds` — ceiling on **one** submit attempt. Default `13`. Chosen so the whole retry budget fits the deadline: 3 attempts × 13s plus 3s of back-off is 42s, inside `total_timeout_seconds` of 45. It was 30s, which meant the deadline cut the sequence off after one attempt and part of a second, so `max_retries` described attempts that never ran.
* `health_timeout_seconds` — timeout for the mapi-lite probe behind `GET /ready`. Default `2`. Must be **less than 3**, and is rejected at startup otherwise: readiness probes allow a few seconds at most (Docker's health check defaults to `--timeout=3s`), and a slower probe would be killed by the caller before it answered, marking the instance unready on every check even while mapi-lite is fine. The probe's verdict is cached for this long, so `/ready` — which is unauthenticated and exempt from rate limiting — cannot be used to flood mapi-lite.
* `max_retries` — how many times a submit is retried after a transient failure (an HTTP 5xx, a transport error, or an attempt that ran out of `timeout_seconds`). Default `2`, so three attempts, with linear back-off of 1s then 2s (capped at 5s). Resubmitting is safe: mapi-lite answers an already-known transaction with success — which is also how a retry can *settle* an attempt that timed out, turning `broadcast_outcome_unknown` back into a plain success.

  Raising this only buys attempts that fit inside `total_timeout_seconds`. If they do not, the service logs a warning at startup naming how many will actually start, and carries on — the combination is legal, it just does less than it asks for.
* `total_timeout_seconds` — ceiling on a whole submit: every attempt and every back-off between them. Default `45`; must be at least `timeout_seconds`. This is the worst case a `POST /fund` caller — and the actix worker serving it — can be held by an unresponsive mapi-lite, so it is the number to set first and then fit the other two inside.

  On expiry the caller gets `broadcast_outcome_unknown` (HTTP 504) if an attempt was still in flight, because cancelling a request says nothing about what the server did with it and the transaction may be on the network; the service reserves that transaction's inputs in response — see [When the outcome is unknown](SupportedEndpoints.md#when-the-outcome-is-unknown). If instead the deadline was consumed by back-off between attempts that were all refused outright, nothing was ever delivered and the caller gets `broadcast_failed` (HTTP 502) with no reservation.

**The three interact.** The worst case is `timeout_seconds × (max_retries + 1)` plus back-off, and `total_timeout_seconds` cuts it off there. Set the deadline to the longest a `/fund` call may take, then choose the other two to fit inside it; the startup warning tells you when they do not.

When the section is present the service:

* Logs at startup: `mapi-lite integration configured (base_url=...): funding transactions will be broadcast via mapi-lite`. Without the section the line reads `mapi-lite not configured: funding transactions will be broadcast via the 'woc' blockchain interface` (naming whichever `interface_type` is configured).
* Probes mapi-lite at startup (`GET /mapi/feeQuote`) and **warns** if it is unreachable, without refusing to start. Funding will fail until mapi-lite is reachable, but the read paths — `/status`, balances, UTXO refreshes — do not depend on it and keep serving, and the service recovers on its own when mapi-lite returns. Refusing to start would instead put the container in a restart loop driven by its own health check, taking the read paths down with it.
* Probes mapi-lite for `GET /ready` and returns HTTP 503 when the probe fails, reusing a verdict for up to `health_timeout_seconds`. `GET /health` is unaffected. See [Readiness check](SupportedEndpoints.md#readiness-check).
* Reports `"broadcaster": "mapi-lite"` in `GET /status`.
* Submits each funding transaction as a one-element batch to `POST /mapi/txs`, with the merkle proof declined (no callbacks are wanted). A rejection by mapi-lite or the node surfaces to the caller as `broadcast_failed` (HTTP 502), with the reason in the service log. An answer that never arrives, or one this client cannot read, surfaces as `broadcast_outcome_unknown` (HTTP 504) instead, because the transaction may have reached the network.

The mapi-lite server must be pointed at the same network as `network_type`, since the funding transactions are signed for that network. Because mapi-lite talks to a node directly, this is also a way to broadcast on regtest while still reading chain state through another interface.

Building the service with this integration requires read access to the private `mapi-lite` repository — see [Dependencies.md](Dependencies.md).

## [dynamic_config]

Path to the file used to persist clients added at runtime via `POST /client`. Dynamically added clients may include an `api_key` field in the same format as static `[[client]]` entries.

```toml
[dynamic_config]
filename = "./data/dynamic.toml"
```

## [[client]]

Static client configuration. Clients can also be added at runtime via the REST API.

```toml
[[client]]
client_id = "id1"
wif_key = "cW1ciwAgTLs2EGa6cZHpf...kvq72s15rbiUonkrQAhDU4FG"
api_key = "your-client-secret"
```

* `client_id` — identifier used in API requests
* `wif_key` — WIF private key for the client's funding wallet
* `api_key` — optional shared secret for this client's API endpoints

When `api_key` is set for a client, requests for that client must include either `Authorization: Bearer <api_key>` or `X-API-Key: <api_key>`. Clients without an `api_key` remain unauthenticated; restrict them via network isolation. The service logs a warning at startup listing clients without an `api_key`.

## Secret management

Avoid storing literal secrets in TOML when possible. The service supports:

### `env:VAR_NAME` references

Use the `env:` prefix to load a value from the process environment at startup:

```toml
[web_interface]
admin_api_key = "env:FS_ADMIN_API_KEY"

[[client]]
client_id = "id1"
wif_key = "env:MY_CLIENT_WIF"
api_key = "env:MY_CLIENT_API_KEY"
```

If a referenced variable is not set, startup fails with a clear error.

### Environment overrides

These variables take precedence over config file values:

| Variable | Overrides |
|----------|-----------|
| `FS_ADMIN_API_KEY` | `web_interface.admin_api_key` |
| `FS_CLIENT_{CLIENT_ID}_WIF` | `wif_key` for that client (`CLIENT_ID` is uppercased; non-alphanumeric characters become `_`) |
| `FS_CLIENT_{CLIENT_ID}_API_KEY` | `api_key` for that client |
| `FS_RPC_USER` / `FS_RPC_PASSWORD` | `blockchain_interface.rpc_user` / `rpc_password` |
| `FS_MAPI_LITE_AUTH_TOKEN` | `mapi_lite.auth_token` (include the scheme, e.g. `Bearer <secret>`) |

Example: for `client_id = "id1"`, set `FS_CLIENT_ID1_WIF`.

### Plaintext warnings

When WIF keys, client `api_key`, `admin_api_key`, `rpc_password`, or `mapi_lite.auth_token` are stored as literal values in config files, the service logs a warning at startup. Literal values still work for local development.

Dynamic clients added via `POST /client` can use `wif_env` and `api_key_env` instead of `wif` and `api_key`; the service stores `env:VAR` references in the dynamic config file rather than the secret values.