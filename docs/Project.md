# Project status

High-level status and known limitations for the Financing Service Rust implementation (v2.1.0).

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

The Rust service uses the [`chain-gang`](https://github.com/nchain-innovation/chain-gang) library for blockchain access (WhatsOnChain, UaaS, or test interface) and wallet operations.

## Current architecture

```
Client App  ──REST──▶  Actix Web API  ──▶  Service  ──▶  BlockchainInterface
Admin       ──REST──▶       │                │
                            │                ├── Client wallets (WIF keys)
                            │                └── dynamic.toml (runtime clients)
                            └── /health (liveness, no auth)
```

| Module | Role |
|--------|------|
| `main.rs` | Config load, HTTP server, periodic UTXO refresh |
| `rest_api.rs` | REST handlers |
| `service.rs` | Orchestration, broadcast, client management |
| `client.rs` | UTXO selection, transaction construction and signing |
| `auth.rs` | Per-client API key verification |
| `blockchain_factory.rs` | Pluggable blockchain backends |
| `dynamic_config.rs` | Persist runtime-added clients |
| `rate_limit.rs` | Per-IP HTTP rate limiting middleware |

## Implemented

* REST API for funding, balance, address, client management, status, and health
* JSON request/response bodies with typed serde DTOs
* Dynamic client add/remove via `POST /client` and `DELETE /client/{id}`
* Optional per-client `api_key` authentication
* Optional `admin_api_key` for `POST /client`
* Secret references via `env:VAR`, environment overrides, and `wif_env` / `api_key_env` on `POST /client`
* Configurable per-IP HTTP rate limiting with `/health` exempt
* Balance checks against total wallet balance; funding combines multiple UTXOs when needed; balance endpoint refreshes from chain on each request
* Docker image with `/health` liveness check
* CI: build, test, `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo audit`
* Pinned `chain-gang` git dependency and committed `Cargo.lock` for reproducible builds
* 64 automated tests (unit, integration, REST API)

## Known limitations

* **Multi-tx partial failure** — if a later broadcast fails in `multiple_tx` mode, earlier transactions remain on-chain. The service resyncs UTXO state from the blockchain and returns an error listing successful transaction ids.
* **Concurrent fund requests for the same client** — funding for a given `client_id` still serializes on that client's UTXO lock. Different clients can fund concurrently.
* **No HTTPS** — expected to be handled at the deployment layer (reverse proxy, firewall).

## Open items

None at present.

## Related documentation

* [SupportedEndpoints.md](SupportedEndpoints.md) — REST API reference
* [Configuration.md](Configuration.md) — service and client configuration
* [LockingScripts.md](LockingScripts.md) — generating locking scripts for `/fund`
* [Development.md](Development.md) — build, test, and CI
* [README.md](../README.md) — overview and getting started
