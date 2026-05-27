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
# api_key = "your-secret-key"
```

When `APP_ENV=docker`, the service listens on `0.0.0.0` regardless of `address`.

### Authentication

Set `api_key` to require a shared secret on all endpoints except `/` and `/health`. Clients must send either:

* `Authorization: Bearer <api_key>`
* `X-API-Key: <api_key>`

When `api_key` is omitted, authentication is disabled. In that case, restrict access with network isolation (for example, bind to `127.0.0.1` and place the service behind a reverse proxy or private network). The service logs a warning at startup when authentication is disabled.

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

Path to the file used to persist clients added at runtime via `POST /client`.

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
```

* `client_id` — identifier used in API requests
* `wif_key` — WIF private key for the client's funding wallet