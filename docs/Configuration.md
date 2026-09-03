# Financing Service - Configuration

Configuration for this service can be found in `data/financing-service.toml`. The file is read when the service starts.

Alternatively, set the `FS_CONFIG` environment variable to a JSON-encoded config object.

The file is composed of the following sections:

## [blockchain_interface]

Configures the blockchain interface. Supported `interface_type` values: `woc`, `uaas`, `test`.

```toml
[blockchain_interface]
interface_type = "woc"
network_type = "testnet"
# url = "http://localhost:5010"  # required for uaas
```

Supported `network_type` values: `mainnet`, `testnet`, `stn`.

### Which interface reaches which network

The interface you choose constrains which networks you can reach, and this is usually the deciding factor for a local deployment.

| `interface_type` | Reaches | mainnet | testnet | stn | regtest |
|---|---|---|---|---|---|
| `woc` | WhatsOnChain, a public API | ✅ | ✅ | ✅ | ❌ |
| `uaas` | a UTXO as a Service instance (set `url`) | ✅ | ✅ | ✅ | ❌ |
| `test` | nothing — an in-process stub | — | — | — | — |

**`regtest` is not supported by any interface.** `network_type = "regtest"` is not an accepted value either, so it fails at startup with `unable to decode network`. There is currently no way to run this service against a local regtest chain; see [issue #44](https://github.com/nchain-innovation/financing-service/issues/44).

**`test` is a fixture, not a backend.** It is an in-process stub used by the unit tests, with a UTXO set injected directly by the test harness. It has no network of its own, so the `network_type` you set alongside it only affects address encoding. It is also not runnable as a configured backend today: a default-constructed stub panics on the first balance query (see [chain-gang#139](https://github.com/nchain-innovation/chain-gang/issues/139)).

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

When enabled, excess requests receive HTTP 429 with a JSON error body. `/health` is exempt so container orchestration probes are not throttled.

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

Configures the period between UTXO refresh requests from the blockchain (in seconds).

```toml
[service]
utxo_refresh_period = 60
```

## [idempotency]

Optional. Controls how long `POST /fund` idempotency records are retained; see [Idempotency](SupportedEndpoints.md#idempotency) for what they do. Both fields have defaults, so the section can be omitted entirely.

```toml
[idempotency]
ttl_seconds = 600
max_entries = 10000
```

* `ttl_seconds` — how long a completed record stays replayable. Should comfortably exceed your clients' retry window; too short and a legitimate retry funds a second time. Default `600`.
* `max_entries` — upper bound on retained records, so a client sending a fresh key on every request cannot grow the store without limit. When full, the oldest record is dropped. Default `10000`.

Records are held **in memory only** and are lost when the service restarts, so a retry that spans a restart can still produce a second funding transaction.

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

Example: for `client_id = "id1"`, set `FS_CLIENT_ID1_WIF`.

### Plaintext warnings

When WIF keys, client `api_key`, or `admin_api_key` are stored as literal values in config files, the service logs a warning at startup. Literal values still work for local development.

Dynamic clients added via `POST /client` can use `wif_env` and `api_key_env` instead of `wif` and `api_key`; the service stores `env:VAR` references in the dynamic config file rather than the secret values.