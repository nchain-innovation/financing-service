# Supported endpoints

The service listens on the port configured in `data/financing-service.toml` (default `8080`).

When a client has an `api_key` configured, that client's endpoints require authentication via either:

* `Authorization: Bearer <api_key>`
* `X-API-Key: <api_key>`

The key must match the `client_id` being accessed. Using another client's key returns `401`.

This applies to `POST /fund`, `GET /client/{client_id}/balance`, `GET /client/{client_id}/address`, and `DELETE /client/{client_id}` when that client has an `api_key`. `/`, `/health`, and `/status` remain unauthenticated.

`POST /client` requires `web_interface.admin_api_key` when configured. Send the admin key via `Authorization: Bearer <admin_api_key>` or `X-API-Key: <admin_api_key>`. The request body may include an optional `api_key` field that is stored for the new client. See [Configuration](Configuration.md).

Missing or invalid credentials return HTTP `401`:

```json
{"code": "unauthorized", "description": "Unauthorized"}
```

When `[web_interface.rate_limit]` is enabled, excess requests from the same IP receive HTTP `429`:

```json
{"code": "rate_limited", "description": "Rate limit exceeded, retry in 1s"}
```

All JSON error responses share this shape:

```json
{"code": "invalid_request", "description": "Error message"}
```

The `code` is a stable, machine-readable classification, and the HTTP status is derived from it — see [Error responses](#error-responses) for the full mapping.

Successful JSON responses use HTTP status `200`.

## Index

`GET /`

Returns a plain-text service identifier.

```bash
curl http://127.0.0.1:8080/
```

```
Financing Service REST API
```

## Health check

`GET /health`

Liveness probe for Docker and orchestrators. Does not check blockchain connectivity.

```bash
curl http://127.0.0.1:8080/health
```

```json
{"status": "ok"}
```

The Docker image includes a `HEALTHCHECK` that calls this endpoint.

## Service status

`GET /status`

Returns the current service status.

```bash
curl http://127.0.0.1:8080/status
```

```json
{
    "version": "4.0.0",
    "blockchain_status": "Connected",
    "blockchain_update_time": "2024-11-05 14:42:29"
}
```

`blockchain_status` can be one of:

* `Unknown` — the service has started but not yet connected to the blockchain
* `Failed` — the service failed to connect to the blockchain
* `Connected` — the service is connected to the blockchain

When no update has occurred yet, `blockchain_update_time` is `"None"`.

## Fund transactions

`POST /fund`

Creates one or more funding transactions. Request body (JSON):

| Field | Type | Description |
|-------|------|-------------|
| `client_id` | string | Client whose wallet funds the transaction |
| `satoshi` | number | Value in satoshis for each funding output |
| `no_of_outpoints` | number | Number of funding outpoints to provide |
| `multiple_tx` | boolean | If true and `no_of_outpoints` > 1, create separate transactions |
| `locking_script` | string | Hex-encoded locking script for the funding outputs |

Requires the `client_id` client's `api_key` when configured.

```bash
curl -H "Authorization: Bearer your-client-api-key" \
     -H "Content-Type: application/json" \
     --request POST \
     --data '{"client_id":"client1","satoshi":123,"no_of_outpoints":1,"multiple_tx":false,"locking_script":"76a914ddc574807c3035ab43553a22c0b9df1f55737fae88ac"}' \
     http://127.0.0.1:8080/fund
```

```json
{
    "outpoints": [{
        "hash": "11e1128551854896dba1af5ebd75f7fb712ae88684cae59e86f89b158de86697",
        "index": 1,
        "satoshi": 123,
        "locking_script": "76a914ddc574807c3035ab43553a22c0b9df1f55737fae88ac"
    }],
    "txs": [{"tx": "0100000001..."}]
}
```

`satoshi` and `locking_script` describe the output this outpoint refers to. **They are read from the transaction that was broadcast, not echoed from the request**, so a client can verify what was actually paid rather than assume the request was honoured.

That matters when spending the outpoint: a BSV (BIP-143) signature commits to both the previous output's value and its locking script, so if either differed from what the client assumed, the signature would not verify and the node would reject the spending transaction with a script error that says nothing about the funding value being wrong.
### Locking scripts per outpoint

`POST /fund` accepts the locking script in either of two forms. Exactly one is required.

**One script for every outpoint** — the original form:

```json
{"client_id": "client1", "satoshi": 123, "no_of_outpoints": 3,
 "multiple_tx": false, "locking_script": "76a914...88ac"}
```

All three outputs pay the same script, so all three outpoints are spendable by the same key. That is only what you want when a single owner takes all of them.

**One script per outpoint** — for independently spendable outputs:

```json
{"client_id": "client1", "satoshi": 123, "multiple_tx": false,
 "locking_scripts": ["76a914aa...88ac", "76a914bb...88ac", "76a914cc...88ac"]}
```

The number of outpoints comes from the list length. `no_of_outpoints` may be omitted; if given it must agree with the length, and a mismatch is rejected rather than one silently winning.

This matters when each funded output is spent by a different transaction. Outputs sharing one key are each spendable by the other's input, so one becomes a double-spend of the other. With `locking_scripts` a single funding transaction can serve N independent spends, instead of needing N separate `POST /fund` calls and N funding transactions on chain.

With `multiple_tx: true` the scripts are paid one per transaction rather than one per output of a single transaction.

Rejected as `invalid_request`: supplying both fields, supplying neither, an empty `locking_scripts` list, and a `no_of_outpoints` that disagrees with the list length.

### Idempotency

`POST /fund` broadcasts a funding transaction and then returns the outpoint in the response body. If that response is lost — a client timeout, a dropped connection — the transaction is already on chain and the client has no record of the outpoint. A plain retry funds a **second** time, and the first output is stranded: locked to a key the client may no longer be able to name.

Send an optional `idempotency_key` to prevent that:

```json
{"client_id": "client1", "satoshi": 123, "no_of_outpoints": 1, "multiple_tx": false,
 "locking_script": "76a914...88ac", "idempotency_key": "any-unique-string"}
```

The first call reserves the key and, once the transaction is broadcast, retains the response against it. A retry carrying the same key replays that response without funding again. Omitting the field preserves the previous behaviour exactly.

| Situation | Result |
|---|---|
| New key | Funds normally, and the response is retained |
| Same key, same request, already funded | Replays the first response with `"replayed": true`; nothing is broadcast |
| Same key, still being processed | `422` `key_in_progress` — retry shortly |
| Same key, materially different request | `422` `idempotency_key_reused` — use a fresh `idempotency_key` |
| Refused before anything was built (`insufficient_balance`, `no_suitable_utxo`) | The key is freed, so it can be retried |

**A replayed response is marked.** It carries `"replayed": true`; an ordinary funding response omits the field entirely, so read its absence as false.

```json
{"outpoints": [...], "txs": [...], "replayed": true}
```

This matters because a replay is otherwise indistinguishable from a fresh funding: same status, same shape, same outpoints. If you reuse a key by mistake — meaning "fund me again" rather than "tell me how that went" — you are handed outpoints you were already given, and probably already spent. Signing over a spent output fails at broadcast with a script error that says nothing about why. **Treat an unexpected `"replayed": true` as an error in your own key generation, and fail on it immediately.**

A partly-successful `multiple_tx` batch is retained too, and a retry is answered the same way as the first call — the transactions that reached the chain are reported rather than being funded a second time.

Keys are scoped per `client_id`, so two clients may choose the same string without colliding.

**Two limits worth knowing:**

* **Records are held in memory and are lost when the service restarts.** A retry that spans a restart can still produce a second funding transaction. Retention is bounded by `[idempotency] ttl_seconds` (default 600) and `max_entries` (default 10000); see [Configuration](Configuration.md).
* **After a failure that may have spent something, the key is not freed** — only the refusals listed above free it. Any other failure leaves the key reserved until its TTL expires, and a retry with it returns `key_in_progress`. This is deliberate: a broadcast may have reached the node before the connection dropped, so releasing the key could cause the duplicate this mechanism exists to prevent. Use a fresh `idempotency_key` if you need to retry sooner.

### Error responses

Every error response carries a machine-readable `code` alongside the human-readable `description`:

```json
{
    "code": "insufficient_balance",
    "description": "Insufficient client balance: 900 satoshi available, 873 required."
}
```

**The `code` is the contract; the `description` is prose for humans and may be reworded at any time.** Clients should branch on `code` and log `description`. Treat an unrecognised code the way you would have treated the response before codes existed — the set may grow.

The status is derived from the code, so a caller that cannot read the body — a proxy, a gateway, a retry policy, an alerting rule — can still classify the failure: **5xx is worth retrying unchanged, 4xx needs the caller or an operator to change something.** `partial_broadcast` is the sole exception to that reading: it is a 422 precisely because the body must be read.

| `code` | HTTP | Meaning | Caller action |
|---|---|---|---|
| `insufficient_balance` | 409 | Total balance cannot cover the request | Top the wallet up; retryable after that |
| `no_suitable_utxo` | 409 | Balance is sufficient but no combination of UTXOs fits | Split the wallet's UTXOs; retryable after that |
| `unknown_client` | 404 / 400 | No such `client_id`. 404 where the id is a path segment, 400 where it is a body field | Fix configuration; never retryable as-is |
| `client_exists` | 409 | `client_id` is already configured | `POST /client` only |
| `invalid_request` | 400 | The request is malformed | Fix the request; never retryable unchanged |
| `broadcast_failed` | 502 | Transaction built and signed, but the node rejected it or was unreachable | Retry may succeed |
| `partial_broadcast` | 422 | Some of the requested transactions broadcast, some did not | **Read the body** — the successful ones are in it |
| `chain_unavailable` | 503 | The blockchain interface could not be reached | Retryable |
| `internal` | 500 | Unexpected internal failure | Report it |
| `unauthorized` | 401 | Authentication missing or invalid | Fix credentials |
| `rate_limited` | 429 | Request rate exceeded | Retry after the interval in `description` |
| `key_in_progress` | 409 | A request with this `idempotency_key` is still being processed | Retry shortly |
| `idempotency_key_reused` | 409 | This `idempotency_key` was used with a different request | Use a fresh `idempotency_key` |

Examples:

* Unauthorized — `{"code": "unauthorized", "description": "Unauthorized"}` (missing or invalid `api_key` for the client)
* Unknown client — `{"code": "unknown_client", "description": "Unknown client_id client1"}`
* Insufficient total balance — `{"code": "insufficient_balance", "description": "Insufficient client balance: 900 satoshi available, 873 required."}`
* No suitable UTXO set — `{"code": "no_suitable_utxo", "description": "Unable to select UTXOs for funding transaction: largest UTXO is 300 satoshi, 873 required including fees."}`
* Invalid input — `{"code": "invalid_request", "description": "Invalid satoshi value '0'"}`
* Partial `multiple_tx` failure — HTTP 422 with successful transactions included when some broadcasts succeed before a later failure:

```json
{
    "code": "partial_broadcast",
    "description": "Failed to broadcast funding transaction 2 of 3: Failed to broadcast funding transaction. 1 transaction(s) were broadcast successfully: 11e11285...",
    "outpoints": [{"hash": "11e1128551854896dba1af5ebd75f7fb712ae88684cae59e86f89b158de86697", "index": 1, "satoshi": 123, "locking_script": "76a914ddc574807c3035ab43553a22c0b9df1f55737fae88ac"}],
    "txs": [{"tx": "0100000001..."}]
}
```

Funding combines multiple UTXOs when no single input is large enough, as long as total wallet balance covers outputs and fees.

## Add client

`POST /client`

Add a client at runtime. The client is persisted to the dynamic config file. Requires `web_interface.admin_api_key` when configured.

Request body (JSON):

| Field | Type | Description |
|-------|------|-------------|
| `client_id` | string | Identifier for the new client |
| `wif` | string | WIF private key for the client's funding wallet (use `wif` or `wif_env`) |
| `wif_env` | string | Environment variable name containing the WIF; stored as `env:VAR` in dynamic config |
| `api_key` | string | Optional shared secret for this client's API endpoints |
| `api_key_env` | string | Environment variable name containing the client API key; stored as `env:VAR` in dynamic config |

Provide either `wif` or `wif_env`, and either `api_key` or `api_key_env` (not both pairs).

```bash
curl -H "Authorization: Bearer your-admin-api-key" \
     -H "Content-Type: application/json" \
     --request POST \
     --data '{"client_id":"client15","wif":"cVLcPuZMfnNNcaU...................oLh3piTnX9WCndRqWh","api_key":"your-client-api-key"}' \
     http://127.0.0.1:8080/client
```

```json
{"status": "Success"}
```

Common error responses:

* Unauthorized — `{"code": "unauthorized", "description": "Unauthorized"}` (missing or invalid admin key)

## Delete client

`DELETE /client/{client_id}`

Remove a dynamically added client. Requires the target client's `api_key` when configured.

```bash
curl -H "Authorization: Bearer your-client-api-key" \
     -X DELETE http://127.0.0.1:8080/client/client1
```

```json
{"status": "Success"}
```

## Get address

`GET /client/{client_id}/address`

Return the funding address for a client. Requires the target client's `api_key` when configured.

```bash
curl -H "Authorization: Bearer your-client-api-key" \
     http://127.0.0.1:8080/client/client1/address
```

```json
{
    "address": "mfxjfLTXLUcCxMDojqRejpfKnF9WhRG5BK"
}
```

## Client balance

`GET /client/{client_id}/balance`

Return the satoshi balance for a client. Requires the target client's `api_key` when configured.

The service refreshes balance and UTXO state from the blockchain on each request before responding.

```bash
curl -H "Authorization: Bearer your-client-api-key" \
     http://127.0.0.1:8080/client/client1/balance
```

```json
{
    "confirmed": 99904,
    "unconfirmed": 95162
}
```
