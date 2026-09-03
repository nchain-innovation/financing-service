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
{"description": "Unauthorized"}
```

When `[web_interface.rate_limit]` is enabled, excess requests from the same IP receive HTTP `429`:

```json
{"description": "Rate limit exceeded, retry in 1s"}
```

All JSON error responses use HTTP status `422` with this shape:

```json
{"description": "Error message"}
```

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
    "version": "3.0.0",
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

Common error responses:

* Unauthorized — `{"description": "Unauthorized"}` (missing or invalid `api_key` for the client)
* Unknown client — `{"description": "Unknown client_id client1"}`
* Insufficient total balance — `{"description": "Insufficent client balance: 900 satoshi available, 873 required."}`
* No suitable UTXO set — `{"description": "Unable to select UTXOs for funding transaction: largest UTXO is 300 satoshi, 873 required including fees."}`
* Invalid input — `{"description": "Invalid satoshi value '0'"}`
* Partial `multiple_tx` failure — HTTP 422 with successful transactions included when some broadcasts succeed before a later failure:

```json
{
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

* Unauthorized — `{"description": "Unauthorized"}` (missing or invalid admin key)

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
