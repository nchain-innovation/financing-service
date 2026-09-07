# Project status

High-level status and known limitations for the Financing Service Rust implementation (v3.0.0).

For API details see [SupportedEndpoints.md](SupportedEndpoints.md). For configuration see [Configuration.md](Configuration.md). For build and test instructions see [Development.md](Development.md).

## History

| Milestone | Date |
|-----------|------|
| Initial design document | September 2022 |
| Python implementation | October 2022 |
| Rust implementation | October 2022 – October 2023 |
| Overlay network updates (client names removed from status) | October 2024 |
| REST API with JSON body for `/fund` | 2024 |
| Per-client API key authentication | 2026 |
| Secret management (`env:VAR`, overrides, dynamic client env refs) | 2026 |
| Per-IP HTTP rate limiting | 2026 |
| Live balance refresh and pre-fund UTXO resync | 2026 |
| Multi-tx partial failure structured responses | 2026 |
| Same-client concurrent funding (plan-then-commit) | 2026 |

The Rust service uses the [`chain-gang`](https://github.com/nchain-innovation/chain-gang) library for blockchain access (WhatsOnChain, UaaS, or test interface) and wallet operations.

## Current architecture

```
Client App  ──REST──▶  Actix Web API  ──▶  Service  ──▶  BlockchainInterface
Admin       ──REST──▶       │                │
                            │                ├── Per-client wallets (Arc<RwLock<Client>>)
                            │                └── dynamic.toml (runtime clients)
                            ├── rate_limit (per-IP, /health exempt)
                            └── /health (liveness, no auth)
```

| Module | Role |
|--------|------|
| `main.rs` | Config load, HTTP server, periodic UTXO refresh |
| `rest_api.rs` | REST handlers, auth gates |
| `service.rs` | Orchestration, funding flow, client management |
| `client.rs` | UTXO selection, transaction construction, plan/commit |
| `auth.rs` | Per-client and admin API key verification |
| `secrets.rs` | `env:VAR` resolution and plaintext warnings |
| `responses.rs` | Typed JSON request/response DTOs |
| `config.rs` | TOML and environment config loading |
| `blockchain_factory.rs` | Pluggable blockchain backends |
| `dynamic_config.rs` | Persist runtime-added clients |
| `rate_limit.rs` | Per-IP HTTP rate limiting middleware |
| `telemetry.rs` | Tracing subscriber and OpenTelemetry OTLP export |

## Implemented

* REST API for funding, balance, address, client management, status, and health
* JSON request/response bodies with typed serde DTOs
* Dynamic client add/remove via `POST /client` and `DELETE /client/{id}`
* Optional per-client `api_key` authentication
* Optional `admin_api_key` for `POST /client`
* Secret references via `env:VAR`, environment overrides, and `wif_env` / `api_key_env` on `POST /client`
* Optional OpenTelemetry trace export via OTLP (configurable, disabled by default)
* Configurable per-IP HTTP rate limiting with `/health` exempt
* Balance checks against total wallet balance; funding combines multiple UTXOs when needed; balance endpoint refreshes from chain on each request; `multiple_tx` partial failures return structured successful transaction data; concurrent fund requests for the same client use read-only planning and commit UTXO updates only after broadcast
* Docker image with `/health` liveness check
* CI: build, test, `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo audit`
* Pinned `chain-gang` git dependency and committed `Cargo.lock` for reproducible builds
* 114 automated tests (unit, integration, REST API, system requirements)

## Known limitations

* **Multi-tx partial failure** — in `multiple_tx` mode, earlier successful broadcasts remain on-chain if a later step fails. The service resyncs UTXO state and returns HTTP 422 with code `partial_broadcast` and `description`, plus `outpoints` and `txs` for any transactions that were broadcast successfully.
* **No HTTPS** — expected to be handled at the deployment layer (reverse proxy, firewall).
* **Rate limiting behind a reverse proxy** — limits apply to the proxy's IP unless the proxy forwards the original client address and a custom key extractor is added.
* **Same-client concurrent funding** — concurrent requests for one client can contend for the same UTXO; one may fail and trigger a resync. Different clients are not blocked by each other.

## Open items

None at present.

## Related documentation

* [SupportedEndpoints.md](SupportedEndpoints.md) — REST API reference
* [Configuration.md](Configuration.md) — service and client configuration
* [LockingScripts.md](LockingScripts.md) — generating locking scripts for `/fund`
* [Development.md](Development.md) — build, test, and CI
* [SystemRequirements.md](SystemRequirements.md) — system requirements and verification methods
* [README.md](../README.md) — overview and getting started
