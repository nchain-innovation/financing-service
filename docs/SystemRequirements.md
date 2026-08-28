# System requirements

System requirements for the Financing Service Rust implementation (v2.2.0), with verification methods for each requirement.

For API behaviour see [SupportedEndpoints.md](SupportedEndpoints.md). For configuration see [Configuration.md](Configuration.md). For implementation status see [Project.md](Project.md).

## Scope

These requirements apply to the Financing Service (FS) as delivered in this repository: a REST API that creates Bitcoin SV funding transaction outpoints for configured client wallets.

**In scope:** REST API, wallet/funding logic, blockchain interfaces (WhatsOnChain, UaaS, test), configuration, authentication, rate limiting, optional OpenTelemetry trace export, Docker deployment, and CI quality gates.

**Out of scope:** HTTPS termination (deployment layer), HD (BIP-32) wallets, client-side funding-transaction caching, trusted-proxy rate-limit key extraction, and rollback of on-chain transactions after partial `multiple_tx` failure.

## Verification

Every requirement below has at least one automated test. Run the full suite with:

```bash
cargo test
```

Requirement IDs appear in test function names (for example `sr_fund_008_*`) or are covered by existing tests cited in the **Verification** column.

## Functional requirements — REST API

| ID | Requirement | Priority | Verification |
|----|-------------|----------|--------------|
| SR-FUNC-001 | The service SHALL expose `GET /` returning plain-text service identifier `Financing Service REST API`. | Must | **AT:** `rest_api::tests::test_index` |
| SR-FUNC-002 | The service SHALL expose `GET /health` returning HTTP 200 and JSON `{"status":"ok"}`. | Must | **AT:** `rest_api::tests::test_health` |
| SR-FUNC-003 | The service SHALL expose `GET /status` returning version, blockchain connection status, and last update time. | Must | **AT:** `rest_api::tests::test_status` |
| SR-FUNC-004 | The service SHALL expose `POST /fund` accepting JSON (`client_id`, `satoshi`, `no_of_outpoints`, `multiple_tx`, `locking_script`) and returning funding `outpoints` and `txs` on success. | Must | **AT:** `rest_api::tests::test_fund_success`, `test_fund_multiple_tx_success` |
| SR-FUNC-005 | The service SHALL expose `GET /client/{client_id}/balance` returning confirmed and unconfirmed satoshi balances. | Must | **AT:** `rest_api::tests::test_balance_success` |
| SR-FUNC-006 | The service SHALL expose `GET /client/{client_id}/address` returning the client's funding address. | Must | **AT:** `rest_api::tests::test_address_success` |
| SR-FUNC-007 | The service SHALL expose `POST /client` to add a client at runtime and persist it to the dynamic config file. | Must | **AT:** `rest_api::tests::sr_func_007_add_client_persists_to_dynamic_config_file`, `test_add_client_success`, `test_add_client_with_wif_env` |
| SR-FUNC-008 | The service SHALL expose `DELETE /client/{client_id}` to remove a dynamically added client. | Must | **AT:** `rest_api::tests::test_delete_client_success` |
| SR-FUNC-009 | Unknown `client_id` on client-scoped endpoints SHALL return HTTP 422 with a descriptive JSON error. | Must | **AT:** `rest_api::tests::test_balance_unknown_client`, `test_address_unknown_client`, `test_fund_unknown_client`, `test_delete_client_unknown` |
| SR-FUNC-010 | Invalid fund request parameters (`satoshi`, `no_of_outpoints`, `locking_script`) SHALL return HTTP 422 with a descriptive JSON error. | Must | **AT:** `rest_api::tests::test_fund_invalid_satoshi`, `test_fund_invalid_no_of_outpoints`, `test_fund_invalid_locking_script` |
| SR-FUNC-011 | Successful JSON responses SHALL use HTTP 200; error JSON responses SHALL use HTTP 422 unless another status is specified (401, 429). | Must | **AT:** `rest_api::tests::sr_func_011_success_and_error_responses_use_expected_status_codes` |

## Functional requirements — funding

| ID | Requirement | Priority | Verification |
|----|-------------|----------|--------------|
| SR-FUND-001 | The service SHALL sign funding transactions using only the configured client WIF; it SHALL NOT access the requesting application's private keys. | Must | **AT:** `client::tests::sr_fund_001_funding_tx_uses_supplied_locking_script` |
| SR-FUND-002 | The service SHALL reject funding when total wallet balance is insufficient for requested outputs and fees. | Must | **AT:** `rest_api::tests::test_fund_insufficient_balance`, `client::tests::test_funding_balance_error_insufficient_total` |
| SR-FUND-003 | The service SHALL combine multiple UTXOs in one transaction when no single UTXO covers the required amount including fees. | Must | **AT:** `rest_api::tests::test_fund_consolidates_multiple_utxos`, `client::tests::test_create_funding_tx_consolidates_multiple_utxos`, `test_funding_balance_error_consolidates_multiple_utxos` |
| SR-FUND-004 | When `multiple_tx` is true and `no_of_outpoints` > 1, the service SHALL create and broadcast separate transactions per outpoint. | Must | **AT:** `service::tests::fund_with_multiple_transactions_broadcasts_each_tx_separately`, `rest_api::tests::test_fund_multiple_tx_success` |
| SR-FUND-005 | Before building a funding transaction, the service SHALL refresh the client's UTXO state from the blockchain. | Must | **AT:** `rest_api::tests::sr_fund_005_fund_endpoint_refreshes_stale_utxo_cache`, `service::tests::refresh_client_chain_state_restores_stale_balance_and_utxo_cache` |
| SR-FUND-006 | On `GET /client/{client_id}/balance`, the service SHALL refresh balance and UTXO state from the blockchain before responding. | Must | **AT:** `rest_api::tests::sr_fund_006_balance_endpoint_refreshes_stale_utxo_cache`, `service::tests::refresh_client_chain_state_restores_stale_balance_and_utxo_cache` |
| SR-FUND-007 | In `multiple_tx` mode, if a later broadcast fails after earlier successes, the service SHALL return HTTP 422 with `description`, and include `outpoints` and `txs` for successfully broadcast transactions when available. | Must | **AT:** `service::tests::partial_broadcast_error_lists_successful_txids`, `responses::tests::partial_funding_error_response_includes_successful_transactions` |
| SR-FUND-008 | After partial `multiple_tx` failure, the service SHALL resync UTXO state from the blockchain. | Must | **AT:** `service::tests::sr_fund_008_partial_multiple_tx_failure_resyncs_chain_state` |
| SR-FUND-009 | Concurrent fund requests for different clients SHALL NOT block each other. | Must | **AT:** `service::tests::concurrent_fund_requests_for_different_clients_do_not_block` |
| SR-FUND-010 | Concurrent fund requests for the same client SHALL use read-only planning under a shared lock and commit UTXO cache updates only after successful broadcast. | Must | **AT:** `client::tests::sr_fund_010_plan_funding_tx_leaves_utxo_cache_unchanged_until_commit`, `service::tests::concurrent_fund_requests_for_same_client_do_not_block` |

## Functional requirements — client management

| ID | Requirement | Priority | Verification |
|----|-------------|----------|--------------|
| SR-CLNT-001 | The service SHALL support multiple clients from static `[[client]]` configuration. | Must | **AT:** `config::tests::sr_clnt_001_config_supports_multiple_static_clients`, `rest_api::tests::sr_clnt_001_service_supports_multiple_configured_clients` |
| SR-CLNT-002 | Adding a duplicate `client_id` SHALL return HTTP 422. | Must | **AT:** `rest_api::tests::test_add_client_duplicate` |
| SR-CLNT-003 | Adding a client with an invalid WIF SHALL return HTTP 422. | Must | **AT:** `rest_api::tests::test_add_client_invalid_wif`, `client::tests::test_invalid_wif_key` |
| SR-CLNT-004 | `POST /client` SHALL accept `wif_env` and `api_key_env` and persist `env:VAR` references instead of secret values. | Must | **AT:** `dynamic_config::tests::sr_clnt_004_and_sr_nfr_008_add_client_persists_to_dynamic_config_file`, `rest_api::tests::test_add_client_with_wif_env` |

## Functional requirements — blockchain

| ID | Requirement | Priority | Verification |
|----|-------------|----------|--------------|
| SR-BCHN-001 | The service SHALL support blockchain backends `woc`, `uaas`, and `test` via configuration. | Must | **AT:** `blockchain_factory::tests::sr_bchn_001_blockchain_factory_supports_woc_test_and_uaas`, `sr_bchn_001_blockchain_factory_rejects_unknown_interface_type` |
| SR-BCHN-002 | The service SHALL support network types `mainnet`, `testnet`, `stn`, and `regtest` (which maps onto testnet, as chain-gang has no regtest variant and regtest shares testnet's base58 version bytes). | Must | **AT:** `config::tests::sr_bchn_002_get_network_supports_mainnet_testnet_stn_and_regtest`, `config::tests::sr_bchn_002_get_network_rejects_unknown_network_type` |
| SR-BCHN-003 | The service SHALL periodically refresh all client UTXO balances at the interval configured by `service.utxo_refresh_period` (seconds). | Must | **AT:** `config::tests::sr_bchn_003_sample_config_sets_utxo_refresh_period` |
| SR-BCHN-004 | `/health` SHALL NOT depend on blockchain connectivity (liveness only). | Must | **AT:** `rest_api::tests::test_health` |

## Security requirements

| ID | Requirement | Priority | Verification |
|----|-------------|----------|--------------|
| SR-SEC-001 | When a client `api_key` is configured, `POST /fund`, `GET /client/{id}/balance`, `GET /client/{id}/address`, and `DELETE /client/{id}` SHALL require matching credentials via `Authorization: Bearer` or `X-API-Key`. | Must | **AT:** `rest_api::tests::test_fund_requires_api_key_when_enabled`, `test_balance_requires_client_api_key_when_enabled`, `sr_sec_001_address_and_delete_require_client_api_key_when_enabled`, `auth::tests::authorize_client_*` |
| SR-SEC-002 | Missing or invalid client credentials SHALL return HTTP 401 with `{"description":"Unauthorized"}`. | Must | **AT:** `rest_api::tests::sr_sec_002_unauthorized_response_uses_standard_json_body`, `auth::tests::authorize_client_rejects_missing_key` |
| SR-SEC-003 | A client's API key SHALL NOT authorize access to another client's endpoints. | Must | **AT:** `rest_api::tests::test_fund_rejects_wrong_client_api_key`, `auth::tests::authorize_client_rejects_wrong_client_key` |
| SR-SEC-004 | When `web_interface.admin_api_key` is configured, `POST /client` SHALL require the admin key. | Must | **AT:** `rest_api::tests::test_add_client_requires_admin_key_when_enabled`, `test_add_client_accepts_admin_bearer_token`, `test_add_client_rejects_wrong_admin_key`, `auth::tests::authorize_admin_*` |
| SR-SEC-005 | API key comparison SHALL use constant-time equality. | Must | **AT:** `auth::unit_tests::constant_time_eq_*` |
| SR-SEC-006 | WIF keys, client API keys, and admin API keys SHALL be loadable via `env:VAR_NAME` references at startup. | Must | **AT:** `secrets::tests::resolve_secret_reads_environment_variable`, `config::tests::resolve_secrets_replaces_env_references` |
| SR-SEC-007 | Missing environment variables referenced by `env:VAR` SHALL cause startup failure with a clear error. | Must | **AT:** `secrets::tests::resolve_secret_errors_when_env_var_missing`, `config::tests::sr_nfr_007_load_config_returns_error_for_invalid_file` |
| SR-SEC-008 | Environment overrides `FS_ADMIN_API_KEY`, `FS_CLIENT_{ID}_WIF`, and `FS_CLIENT_{ID}_API_KEY` SHALL take precedence over config file values. | Must | **AT:** `config::tests::resolve_secrets_applies_client_env_overrides`, `sr_sec_008_resolve_secrets_applies_client_api_key_env_override` |
| SR-SEC-009 | Plaintext secrets in configuration SHALL log a warning at startup (WIF, client `api_key`, `admin_api_key`). | Must | **AT:** `secrets::tests::sr_sec_009_plaintext_secret_fields_detects_literal_secrets` |
| SR-SEC-010 | Error responses SHALL NOT echo WIF or API key values. | Must | **AT:** `rest_api::tests::sr_sec_010_invalid_wif_error_does_not_echo_secret` |
| SR-SEC-011 | `/`, `/health`, and `/status` SHALL remain unauthenticated regardless of client API key configuration. | Must | **AT:** `rest_api::tests::test_health_unauthenticated_when_api_key_enabled`, `test_status_unauthenticated_when_client_api_key_enabled`, `sr_sec_011_index_unauthenticated_when_client_api_key_enabled` |
| SR-SEC-012 | When rate limiting is enabled, excess requests from the same IP SHALL receive HTTP 429 with a JSON error body. | Must | **AT:** `rest_api::tests::test_rate_limit_returns_429_when_burst_exceeded` |
| SR-SEC-013 | `/health` SHALL be exempt from rate limiting when rate limiting is enabled. | Must | **AT:** `rest_api::tests::test_health_is_exempt_from_rate_limit` |

## Configuration and deployment requirements

| ID | Requirement | Priority | Verification |
|----|-------------|----------|--------------|
| SR-CFG-001 | The service SHALL load configuration from `data/financing-service.toml` by default. | Must | **AT:** `config::tests::sr_cfg_001_load_config_reads_toml_file` |
| SR-CFG-002 | The service SHALL accept configuration via the `FS_CONFIG` environment variable (JSON-encoded). | Must | **AT:** `config::tests::sr_cfg_002_get_config_reads_fs_config_json` |
| SR-CFG-003 | When `APP_ENV=docker`, the service SHALL listen on `0.0.0.0` regardless of configured `web_interface.address`. | Must | **AT:** `config::tests::sr_cfg_003_web_bind_address_uses_all_interfaces_in_docker` |
| SR-CFG-004 | Rate limiting configuration with `enabled = true` and invalid `requests_per_second` (zero) SHALL fail validation at startup. | Must | **AT:** `config::tests::rate_limit_config_rejects_zero_requests_per_second_when_enabled` |
| SR-CFG-005 | The Docker image SHALL include a health check against `GET /health`. | Must | **AT:** `system_requirements::tests::sr_cfg_005_dockerfile_healthcheck_calls_health_endpoint` |
| SR-CFG-006 | Logging level SHALL be configurable via `[logging].level`. | Must | **AT:** `config::tests::sr_cfg_006_get_log_level_accepts_configured_levels` |
| SR-CFG-007 | Telemetry configuration SHALL be validated at startup. | Must | **AT:** `config::tests::sr_cfg_007_load_config_validates_telemetry_config` |

## Observability requirements — OpenTelemetry

| ID | Requirement | Priority | Verification |
|----|-------------|----------|--------------|
| SR-TELE-001 | OpenTelemetry trace export via OTLP SHALL be optional and disabled by default. | Must | **AT:** `telemetry::tests::telemetry_defaults_to_disabled_without_otel_exporter_env` |
| SR-TELE-002 | When telemetry export is enabled, the service SHALL create a trace span per HTTP request. | Must | **AT:** `system_requirements::tests::sr_tele_002_main_wraps_tracing_logger` |
| SR-TELE-003 | Telemetry SHALL be configurable via `[telemetry]` and standard `OTEL_*` environment variables. | Must | **AT:** `telemetry::tests::otel_traces_exporter_env_enables_telemetry`, `effective_service_name_prefers_otel_service_name_env`, `effective_otlp_endpoint_uses_standard_env_vars` |
| SR-TELE-004 | Enabled export SHALL attach `service.name` and `service.version` resource attributes to traces. | Must | **AT:** `system_requirements::tests::sr_tele_004_telemetry_sets_service_resource_attributes` |
| SR-TELE-005 | Telemetry behaviour SHALL be documented in Configuration.md and README.md. | Must | **AT:** `system_requirements::tests::sr_tele_005_readme_and_configuration_document_opentelemetry` |

## Non-functional requirements

| ID | Requirement | Priority | Verification |
|----|-------------|----------|--------------|
| SR-NFR-001 | The codebase SHALL pass `cargo fmt --check` without formatting violations. | Must | **AT:** `system_requirements::tests::sr_nfr_001_ci_runs_cargo_fmt_check` |
| SR-NFR-002 | The codebase SHALL pass `cargo clippy -- -D warnings` without warnings. | Must | **AT:** `system_requirements::tests::sr_nfr_002_ci_runs_cargo_clippy_with_warnings_denied` |
| SR-NFR-003 | Dependencies SHALL pass `cargo audit` without known vulnerabilities (or with documented exceptions). | Must | **AT:** `system_requirements::tests::sr_nfr_003_ci_runs_cargo_audit` |
| SR-NFR-004 | All automated tests SHALL pass on supported platforms in CI. | Must | **AT:** `system_requirements::tests::sr_nfr_004_ci_runs_cargo_test` |
| SR-NFR-005 | `Cargo.lock` SHALL be committed for reproducible builds. | Must | **AT:** `system_requirements::tests::sr_nfr_005_cargo_lock_is_committed` |
| SR-NFR-006 | The `chain-gang` dependency SHALL be pinned to a specific git revision. | Must | **AT:** `system_requirements::tests::sr_nfr_006_chain_gang_dependency_is_pinned_to_git_revision` |
| SR-NFR-007 | Startup configuration or secret resolution errors SHALL return an error from `main` rather than panic. | Must | **AT:** `system_requirements::tests::sr_nfr_007_main_returns_error_instead_of_panicking_on_config_failure`, `config::tests::sr_nfr_007_load_config_returns_error_for_invalid_file` |
| SR-NFR-008 | The service SHALL maintain an in-memory UTXO cache per client and persist runtime-added clients to the dynamic config file. | Must | **AT:** `dynamic_config::tests::sr_clnt_004_and_sr_nfr_008_add_client_persists_to_dynamic_config_file`, `sr_nfr_008_remove_client_updates_dynamic_config_file`, `client::tests::sr_fund_010_plan_funding_tx_leaves_utxo_cache_unchanged_until_commit` |

## Constraints and known limitations

| ID | Constraint | Verification |
|----|------------|--------------|
| SR-LIM-001 | HD (BIP-32) wallets are not supported. | **AT:** `system_requirements::tests::sr_lim_001_hd_wallet_support_is_not_implemented` |
| SR-LIM-002 | HTTPS is not provided by the service; TLS is a deployment responsibility. | **AT:** `system_requirements::tests::sr_lim_002_https_is_not_provided_by_the_service_binary` |
| SR-LIM-003 | On-chain transactions from partial `multiple_tx` failure cannot be rolled back. | **AT:** `service::tests::partial_broadcast_error_lists_successful_txids` |
| SR-LIM-004 | Rate limiting behind a reverse proxy applies to the proxy IP unless a custom key extractor is implemented. | **AT:** `rate_limit::tests::sr_lim_004_rate_limit_uses_peer_ip_key_extractor` |
| SR-LIM-005 | Same-client concurrent funding may contend for the same UTXO; one request may fail and trigger resync. | **AT:** `system_requirements::tests::sr_lim_005_same_client_concurrency_is_covered_by_service_tests`, `service::tests::concurrent_fund_requests_for_same_client_do_not_block` |
| SR-LIM-006 | OpenTelemetry export is traces only; metrics and OTLP log export are not supported. | **AT:** `system_requirements::tests::sr_lim_006_opentelemetry_exports_traces_only` |

## Verification summary

Run the full automated verification suite:

```bash
cargo fmt --all -- --check
cargo clippy -- -D warnings
cargo audit
cargo test
```

These commands mirror the GitHub Actions workflow (`.github/workflows/rust.yml`). The `cargo test` step executes all requirement tests above (currently **114** tests).

## Related documentation

* [SupportedEndpoints.md](SupportedEndpoints.md) — REST API reference
* [Configuration.md](Configuration.md) — service and client configuration
* [Project.md](Project.md) — architecture and implementation status
* [Development.md](Development.md) — build, test, and CI
* [README.md](../README.md) — overview and getting started
