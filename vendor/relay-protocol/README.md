# Relay protocol subset

Sources: https://github.com/muentzee/chiaki-rust at commit `7b95e825389ace25307d20c9c89e2d5fc906c474`.

Copied without modification from `crates/chiaki-core/src`: `base64.rs`, `discovery.rs`, `error.rs`, `http.rs`, `random.rs`, `regist.rs`, `rpcrypt.rs`, `rpcrypt/rpcrypt_tables.rs`, `sock.rs`, `stoppipe.rs`, `time.rs`. `Cargo.toml` and the small `src/lib.rs` were written for this subset. No upstream UI is included.

Copyright and authorship remain with the chiaki-rust, chiaki-ng and original Chiaki contributors. The upstream repository distributes this Rust port under AGPL-3.0-only; its LICENSE is preserved here. Original source SPDX headers referencing the Chiaki OpenSSL exception remain unchanged; the original exception text is additionally provided in `AGPL-3.0-only-OpenSSL.txt`. This Rust subset does not use OpenSSL.

Relay's integration is in `src/connections.rs` at the host root. It adds own-panel discovery, registration, browser authorization, Windows DPAPI storage and an isolated QSettings adapter. This folder includes all corresponding source for the incorporated protocol subset. Additional dependencies retain their own licenses; see the host's Rust notices.
