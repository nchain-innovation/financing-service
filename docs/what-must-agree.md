# What must agree with Event and mapi-lite

The Financing Service funds the transactions Event (teranode-event-rs) writes, and broadcasts them through mapi-lite, which it can also read the chain through. This page lists what the three have to agree on, which service owns each fact, and how a mismatch shows ([CS-626](https://teranode.atlassian.net/browse/CS-626)). The same table opens the matching page in teranode-event-rs and mapi-lite (`docs/what-must-agree.md` in each); the section after it is FS's own side. Each setting is described in full in [Configuration.md](Configuration.md).

FS **owns** one fact, its clients and their keys, which Event has to match. It publishes one other fact for its callers: its worst case for a `POST /fund`. For the rest it follows mapi-lite and is checked against it.

## The seven facts

Event (teranode-event-rs), the Financing Service (FS) and mapi-lite each have their own settings, but seven facts are set in more than one of them and have to agree. Each fact has one **owner**, the service whose value is the truth. The others either read it from the owner or are checked against it. CS-604 set this up; CS-623, CS-624 and CS-625 put it in place.

| Fact | Owner | mapi-lite | FS | Event | How a mismatch shows |
|---|---|---|---|---|---|
| **Network** | mapi-lite's nodes | none of its own: its nodes' `getblockchaininfo` chain, published in `GET /ready` as `chain` | `blockchain_interface.network` | `network` | **Startup error** in FS and in Event, naming both networks. Heard later, mapi-lite is **not ready** in their readiness. |
| **Miner ID key** | mapi-lite | `wif_private_key` or `wif_private_key_file`; its public key signs every envelope (`publicKey`, `minerId`) | `mapi_lite.miner_id_public_keys` | `mapi_miner_id_public_keys` | **Startup error** in FS and Event when the list is empty on mainnet or testnet; a warning elsewhere. A key the list does not name: FS reports mapi-lite not ready and refuses funding (`broadcaster_untrusted`); Event fails the request. |
| **mapi-lite auth token** | mapi-lite | `auth_token`, the bare token | `mapi_lite.auth_token`, the bare token | `mapi_auth_token`, the bare token | All three refuse a value holding a space (so `Bearer x`) at **startup**. mapi-lite's `/ready` is unauthenticated, so FS and Event also fetch its signed fee quote, which is not: a 401 is a **startup error**, and heard later makes mapi-lite **not ready** (CS-632). |
| **FS client and key** | FS | — | `[[client]] client_id`, `api_key` | `financing.client_id`, `financing.api_key` | **Not caught before use.** FS answers 401 `unauthorized` or 404 `unknown_client`, and Event's write fails with HTTP 500. |
| **WhatsOnChain budget** | mapi-lite, when FS reads through it | `[woc] max_requests_per_second` (3) | `interface_type = "mapi_lite"` reads through mapi-lite. `woc` reads WhatsOnChain itself, at its own `max_requests_per_second` (3) | — | **Not checked.** With FS on `woc` and mapi-lite's `[woc]` both on, the two budgets add up against WhatsOnChain's free 3 requests a second. |
| **Fee rate** | mapi-lite | `[fee_quote]`, advertised in `GET /mapi/feeQuote`; not enforced on submit | follows the quote; `fees.satoshis_per_kb` until it arrives, `fees.max_satoshis_per_kb` caps it | prices each write from the quote | Nothing to mismatch: both clients read the owner's quote. FS **ignores** a quote above its cap, with a warning. |
| **Timeout chain** | each callee publishes its ceiling; each caller keeps one deadline | `submit_timeout_sec` (10s), published as `submitTimeoutSecs` | `mapi_lite.total_timeout_seconds` (45s); publishes `fund_worst_case_seconds` | `financing.timeout_seconds` (150s); `mapi_timeout_seconds` (derived) | **Warnings**, naming both values. Each per-attempt timeout is derived from the published ceiling, so only a deadline set too short can be wrong. |

### The timeout chain

Every caller has to give up later than the service it calls. Otherwise a hung node is reported by the wrong layer, and work carries on that nobody is waiting for. Outermost first, at the defaults:

| Hop | The caller's bound | Against the callee's | Checked |
|---|---|---|---|
| Event → FS, `POST /fund` | `financing.timeout_seconds` = 150s | FS's `fund_worst_case_seconds` in `GET /status`: 135s with mapi-lite, 120s without. That is three chain reads at `blockchain_interface.timeout_seconds` (30s) plus the broadcast | Event warns when its bound does not exceed FS's |
| FS → mapi-lite, a whole submit | `mapi_lite.total_timeout_seconds` = 45s | one attempt (below), retries included | FS warns when 45s is shorter than one attempt |
| FS → mapi-lite, an attempt | derived: `submitTimeoutSecs` + 3s = 13s, with as many retries as fit inside 45s (two) | `submit_timeout_sec` = 10s | derived, so it always clears the ceiling |
| Event → mapi-lite, an attempt | derived: `submitTimeoutSecs` + 3s = 13s, inside uls-client's 60s bound on a whole broadcast. `mapi_timeout_seconds` fixes it instead | `submit_timeout_sec` = 10s | derived; a fixed value is warned about when it does not exceed the ceiling by 1s |
| mapi-lite → a node | derived: min(`rpc.timeout_sec` (20s), ceiling − 1s) = 9s | — | a submit past the ceiling is answered 504, its outcome unknown, and finished in the background (`submissionsPastCeiling`) |

### Not checked anywhere

Mismatches that nothing catches today. Each one shows only when it is used:

- **Credentials, at the service that holds them.** On mainnet and testnet, mapi-lite and FS require a token or key to be *set* (unless `allow_unauthenticated`), but cannot tell whether a caller holds the same one. The callers check: FS and Event the mapi-lite token (CS-632), and Event its FS client key (CS-631, teranode-event-rs).
- **mapi-lite's nodes against each other.** `chain` comes from the node with the best tip. Nodes on different chains are not refused.
- **mapi-lite's `woc.network` against its nodes' chain**, and FS's client WIFs against `blockchain_interface.network`. A WIF of the other network gives an address on the other network.
- **Event's network against FS's directly.** Both are checked against the same mapi-lite, which makes them agree when they share one.
- **The WhatsOnChain budget**, as above.
- **Lists from the environment.** Neither client's miner ID list can be set by an environment override: Event's `TE__` and FS's `FS__` overrides take scalars only. On mainnet and testnet, where the list is required, it has to be in the file.

## In FS

Settings are TOML keys in the file `FS_CONFIG` names, overridable as `FS__<TABLE>__<KEY>`. An override takes scalars only: neither a list (`miner_id_public_keys`) nor a `[[client]]` entry. A key no setting reads stops startup, naming it (CS-624).

### What it publishes

- **`GET /status` `fund_worst_case_seconds`**: the longest a single-transaction `POST /fund` can take: three chain reads at `blockchain_interface.timeout_seconds` and the broadcast at `mapi_lite.total_timeout_seconds` (or one more chain read without `[mapi_lite]`). At the defaults that is 135s with mapi-lite and 120s without; it is `null` when chain reads have no deadline. Event checks its `financing.timeout_seconds` against it. It does not cover a `multiple_tx` request, or requests queued behind a chain read that keeps failing.
- **`GET /ready`**: `mapi_lite.ok` and `chain.ok`. The body's detail is generic, since it answers anyone; the reason is in the log.

FS does not publish its network.

### Its checks

| Fact | Setting | At startup | Afterwards |
|---|---|---|---|
| Network | `[blockchain_interface] network`: exactly `mainnet`, `testnet`, `regtest` or `stn` | another spelling is refused, naming the one to write; with `[mapi_lite]`, mapi-lite's published `chain` must match, or startup stops naming both. An unreachable mapi-lite is only warned about | `/ready` reports mapi-lite not ready on a mismatch heard later, ahead of any other failure. It also asks mapi-lite's nodes about the oldest confirmed UTXO in the wallets, and fails if they do not know it (CS-619), which catches two private chains both called `regtest` |
| Miner ID key | `[mapi_lite] miner_id_public_keys` (hex) | malformed keys are refused; an empty list with `[mapi_lite]` is an error on mainnet and testnet, a warning elsewhere | a signed answer from another key makes `/ready` fail, and `POST /fund` sends nothing until one verifies again: 503 `broadcaster_untrusted`. The first such answer to a submit is 504 `broadcast_outcome_unknown` (inputs reserved). A quote from it is ignored |
| mapi-lite auth token | `[mapi_lite] auth_token`, the bare token (`env:` reference or `FS__MAPI_LITE__AUTH_TOKEN`) | refused when it holds a space (so `Bearer x` is), without repeating it; a literal value is warned about. The startup probe also fetches mapi-lite's signed fee quote, which needs the token: a 401 stops startup, naming the setting (CS-632) | sent as `Authorization: Bearer <token>`. `/ready` fetches the fee quote too and fails on a 401, ahead of anything else. A broadcast that meets one is 502 `broadcast_failed`, a chain read through mapi-lite 503 `chain_unavailable` |
| Its clients and keys | `[[client]] client_id`, `wif_key`, `api_key`; `[web_interface] admin_api_key`, `allow_unauthenticated` | on mainnet and testnet, unless `allow_unauthenticated`: refused without `admin_api_key`, or with a client lacking an `api_key` | a caller sends `Authorization: Bearer <api_key>` or `X-API-Key`. A wrong key is 401 `unauthorized`; an unknown client 404 `unknown_client`, checked first |
| WhatsOnChain budget | `blockchain_interface.interface_type`: `woc` reads WhatsOnChain at `max_requests_per_second` (3); `mapi_lite` reads through mapi-lite's `/query/address/*` and is paced there | `mapi_lite` needs `[mapi_lite]` and mainnet or testnet, else startup stops | a busy or unconfigured WhatsOnChain behind mapi-lite is 503 `chain_unavailable`, never an empty wallet |
| Fee rate | `[fees] satoshis_per_kb` (100), `max_satoshis_per_kb` (1000) | 0, or a cap below the rate, is refused | with `[mapi_lite]` the quote's standard rate is taken on every refresh sweep (`service.utxo_refresh_period`); one above the cap is ignored with a warning, and one that fails or is unusable leaves the rate in force |
| Timeouts | `blockchain_interface.timeout_seconds` (30), `mapi_lite.total_timeout_seconds` (45) | warns when the deadline is shorter than one attempt against mapi-lite's ceiling (assumed 10s until published), or when chain reads have no deadline | each attempt is `submitTimeoutSecs` + 3s, with as many retries as fit; a new ceiling rebuilds the submit client. The `/ready` probe is bounded at 2s |

### Samples

- `data/financing-service.toml` is testnet with `interface_type = "woc"` and `[mapi_lite]` commented out, so it publishes a worst case of 120s. Being testnet, it starts only with an `admin_api_key` (for instance `FS__WEB_INTERFACE__ADMIN_API_KEY`) and an `api_key` for any client it lists, as its comments say. The `[mapi_lite]` example writes `auth_token = "env:MAPI_AUTH_TOKEN"`, the bare token, and notes that `miner_id_public_keys` is required on mainnet and testnet.
- The uls-rs e2e stack renders FS on regtest with `interface_type = "rpc"` against node1 and mapi-lite's token from the environment. FS's `event` client key is the one Event is given as `financing.api_key`.
