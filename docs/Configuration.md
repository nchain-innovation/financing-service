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

## [service]

Configures the period between UTXO refresh requests from the blockchain (in seconds).

```toml
[service]
utxo_refresh_period = 60
```

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