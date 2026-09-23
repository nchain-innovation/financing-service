//! Guards the invariant that makes the `uls-core` boundary safe to cross.
//!
//! `chain-gang` is referenced twice -- directly by this crate, and
//! transitively through `uls-client` -- so Cargo will happily build two copies
//! whenever those two requirements disagree. Nothing fails to compile at that
//! moment; the graph just quietly stops being sound, and the break arrives
//! later as an error naming what looks like the same type twice. The same
//! goes for `reqwest`, whose error type `uls-client` exposes.
//!
//! teranode-event-rs hit exactly this split twice before adding this check
//! (see its `docs/dependencies.md`), so it is copied here rather than waited
//! for. A chain-gang bump that forgets to move mapi-lite first fails here,
//! with an explanation, instead of in a confusing type error.

#[cfg(test)]
mod tests {
    /// Every `[[package]]` version recorded in the lockfile for `name`.
    fn locked_versions(lock: &str, name: &str) -> Vec<String> {
        let want = format!("name = \"{name}\"");
        lock.split("[[package]]")
            .filter(|entry| entry.lines().any(|line| line.trim() == want))
            .map(|entry| {
                entry
                    .lines()
                    .find_map(|line| line.trim().strip_prefix("version = "))
                    .unwrap_or("<no version>")
                    .trim_matches('"')
                    .to_string()
            })
            .collect()
    }

    fn lockfile() -> String {
        std::fs::read_to_string("Cargo.lock").expect("reading Cargo.lock")
    }

    #[test]
    fn chain_gang_resolves_to_exactly_one_package() {
        let versions = locked_versions(&lockfile(), "chain-gang");
        assert_eq!(
            versions.len(),
            1,
            "chain-gang resolves to {} packages ({versions:?}), not one. This crate's \
             chain-gang requirement and that of the mapi-lite revision uls-client is pinned to \
             have drifted apart. No [patch] fixes a version split -- a patch has to satisfy the \
             dependent's own requirement. Move mapi-lite first, then repin uls-client/uls-core, \
             then bump here. See docs/Dependencies.md.",
            versions.len(),
        );
    }

    #[test]
    fn reqwest_resolves_to_exactly_one_package() {
        // uls-client's error type wraps reqwest::Error, and this crate hands
        // uls-client a reqwest::Client, so both must be the same reqwest.
        let versions = locked_versions(&lockfile(), "reqwest");
        assert_eq!(
            versions.len(),
            1,
            "reqwest resolves to {} packages ({versions:?}), not one. Align the reqwest \
             requirement in Cargo.toml with the one in mapi-lite's workspace.",
            versions.len(),
        );
    }

    #[test]
    fn the_two_uls_crates_come_from_one_mapi_lite_revision() {
        let lock = lockfile();
        let sources: Vec<&str> = lock
            .split("[[package]]")
            .filter(|entry| {
                entry.lines().any(|line| {
                    matches!(line.trim(), "name = \"uls-client\"" | "name = \"uls-core\"")
                })
            })
            .filter_map(|entry| {
                entry
                    .lines()
                    .find_map(|line| line.trim().strip_prefix("source = "))
            })
            .collect();

        assert_eq!(
            sources.len(),
            2,
            "expected uls-client and uls-core in Cargo.lock, got {sources:?}"
        );
        assert_eq!(
            sources[0], sources[1],
            "uls-client and uls-core resolve to different mapi-lite sources: {sources:?}. Both \
             pins in Cargo.toml must name the same rev."
        );
    }

    /// The lowest chain-gang this crate is correct against.
    ///
    /// Two reasons, both about the WhatsOnChain interface.
    ///
    /// 0.11.2 and earlier read UTXOs from `/unspent`, which stops at 1000
    /// entries and offers no way to ask for the rest, so a busy address came
    /// back silently truncated. Measured against mainnet
    /// 1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa: 1000 of 20960 entries, 241612089
    /// of 2296358944 satoshis. For a funding service that is not a cosmetic
    /// difference -- the missing UTXOs are spendable, so the service would
    /// refuse amounts the client can afford. 0.11.3 pages `/unspent/all`.
    ///
    /// 0.11.4 adds `set_max_requests_per_second`, which is what
    /// `blockchain_factory` uses to bound the rate of the requests that paging
    /// turns one call into. Below it the code does not compile, so this floor
    /// records the reason rather than enforcing it; 0.11.3 also fails
    /// `get_utxo` on any address holding an unconfirmed output (CS-462), which
    /// is an ordinary state rather than an edge case.
    const CHAIN_GANG_FLOOR: (u32, u32, u32) = (0, 11, 4);

    #[test]
    fn chain_gang_is_new_enough_to_read_a_whole_utxo_set() {
        let versions = locked_versions(&lockfile(), "chain-gang");
        let [version] = versions.as_slice() else {
            // chain_gang_resolves_to_exactly_one_package reports this case
            return;
        };
        let parts: Vec<u32> = version
            .split('.')
            .map(|p| p.parse().unwrap_or_else(|_| panic!("version {version:?}")))
            .collect();
        let [major, minor, patch] = parts.as_slice() else {
            panic!("version {version:?} is not major.minor.patch");
        };

        assert!(
            (*major, *minor, *patch) >= CHAIN_GANG_FLOOR,
            "chain-gang {version} is below {CHAIN_GANG_FLOOR:?}. Below it a client address \
             with more than 1000 UTXOs reports only the first 1000 and the service refuses \
             funding it could cover, an address holding an unconfirmed output cannot be read \
             at all, and there is no way to bound the rate of the requests paging makes. \
             See docs/Dependencies.md.",
        );
    }

    #[test]
    fn the_parser_reads_a_lockfile_correctly() {
        // Against a fixture, not the live lockfile, so the parser stays
        // tested whatever state the real lock is in.
        const LOCK: &str = r#"
[[package]]
name = "chain-gang"
version = "0.10.1"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "chain-gang"
version = "0.11.1"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "k256"
version = "0.14.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;

        assert_eq!(locked_versions(LOCK, "chain-gang"), ["0.10.1", "0.11.1"]);
        assert_eq!(locked_versions(LOCK, "k256"), ["0.14.0"]);
        assert!(locked_versions(LOCK, "no-such-package").is_empty());
        // Whole-name matches only: `chain` must not match `chain-gang`.
        assert!(locked_versions(LOCK, "chain").is_empty());
        assert!(locked_versions(LOCK, "gang").is_empty());
    }
}
