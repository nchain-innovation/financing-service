# Dependencies

Most of this crate's dependencies come from crates.io and need nothing said about them. Two do not, and they bring one invariant with them. This page covers those.

## `uls-client` and `uls-core` — the mapi-lite crates

| Crate | What it is | Source | Pin |
|---|---|---|---|
| `uls-client` | mapi-lite's typed HTTP client: `MapiClient::submit_transactions`, `fee_quote`, … | `ssh://git@github.com/nchain-innovation/mapi-lite.git` (**private**) | commit `ab3c95846a72976783e5f2d901b69d75768426bc` |
| `uls-core` | The wire types and signed-envelope code both the server and client share | same repo, **same commit** | same |

They exist for the optional mapi-lite transaction broadcaster (`src/broadcaster/mapi.rs`, configured by `[mapi_lite]`). They are compiled in whether or not that section is configured; a build without them is not offered, because a second build variant is more maintenance than the SSH requirement costs.

### Why a commit, not a tag or a version

mapi-lite has one tag (`v0.1.0`) and is not published to crates.io. The client surface this crate uses landed after that tag, so the pin names a `master` commit. It is the same commit `teranode-event-rs` pins, which keeps the two consumers of mapi-lite on one wire contract; the parent `uls-rs` repo's `scripts/preflight.sh` warns when a pin falls behind the mapi-lite checkout it is about to run.

### Why the `ssh://` URL form

Cargo accepts `git = "ssh://git@github.com/org/repo.git"` and not the `git@github.com:org/repo.git` shorthand. `.cargo/config.toml` sets `net.git-fetch-with-cli = true`, so cargo delegates the fetch to the system `git`, which authenticates through your ssh-agent and `~/.ssh/config` exactly as `git clone` would. Cargo's built-in fetcher has weaker SSH support, and this avoids it.

### What you need to build

* **Locally:** an SSH key with read access to `nchain-innovation/mapi-lite` loaded in your agent. `ssh -T git@github.com` should greet you.
* **CI (`.github/workflows/rust.yml`):** a read-only deploy key on the mapi-lite repository, stored in this repository as the `MAPI_LITE_DEPLOY_KEY` secret. The workflow fails fast with a named error if the secret is missing, then loads it with `webfactory/ssh-agent` before the first cargo command.
* **Docker (`build.sh`, `multi-build.sh`):** BuildKit with `--ssh default`, which lends your agent socket to the one `RUN --mount=type=ssh` step that fetches dependencies. The key never enters the image; the builder stage installs `git` and `openssh-client` and pre-trusts `github.com` in `known_hosts`.

## The single-`chain-gang` invariant

`chain-gang` is referenced twice: directly by this crate, and transitively by `uls-core`. Cargo unifies two references into one package only when they name the **same source** and **compatible versions**. If either differs, the graph carries two `chain-gang` packages, and `Hash256` from one is not `Hash256` from the other. `uls-core` hands chain-gang types across the boundary (`ParsedTx.tx`, `compute_merkle_root(&[Hash256])`), so the failure is real: a compile error naming what looks like the same type twice, some distance from the cause.

The two ways it can split, both of which have happened to `teranode-event-rs`:

1. **By source.** A git pin and a crates.io release never unify, even at the same version number. All three repos therefore take chain-gang from **crates.io**.
2. **By version.** `chain-gang` is pre-1.0, so `0.10` and `0.11` are incompatible ranges. If this crate asks for `0.12` while the pinned mapi-lite commit still asks for `0.11`, Cargo resolves both. No `[patch]` can fix this: a patch has to satisfy the dependent's own requirement.

The same applies to `reqwest`, because `uls-client`'s error type wraps `reqwest::Error` and this crate hands `uls-client` a `reqwest::Client`. This crate's `reqwest` requirement therefore matches mapi-lite's major.minor and names no TLS backend of its own; the backend is whatever `uls-client` enables.

`src/dependency_graph.rs` asserts on `Cargo.lock` that `chain-gang` and `reqwest` each resolve to exactly one package and that `uls-client` and `uls-core` share one source. A bump that breaks the invariant fails there, with an explanation, rather than downstream.

### How to bump

**chain-gang:** move mapi-lite first. Once a mapi-lite commit on the new chain-gang exists, in one change here: repin `uls-client` and `uls-core` to that commit, raise `chain-gang` in `Cargo.toml`, run `cargo update -p chain-gang -p uls-client -p uls-core`, run `cargo test`, commit `Cargo.lock`.

**mapi-lite alone** (no chain-gang change): edit both `rev`s to the same new commit, `cargo update -p uls-client -p uls-core`, `cargo test`, commit `Cargo.lock`. Check that `teranode-event-rs` is on the same commit, or the two services will speak subtly different wire contracts to the one mapi-lite they share.

## Everything else

`chain-gang` (crates.io, exact release in `Cargo.lock`) provides the BSV primitives, wallet and the `BlockchainInterface` implementations for WhatsOnChain, UaaS and node RPC. `Cargo.lock` is committed; run `cargo update` only when upgrading on purpose.
