# Milestone 1A.2 results (partial)

**Date:** 2026-11-25  
**Status:** Local harness and health-control implementation verified; external soak and performance acceptance remain pending.

## Implemented

- Independent Tokio `pathweave-test-server` with token authentication, source-IP allow list, connection cap, per-direction transfer limit, session timeout, structured per-connection JSON logs, and Ctrl+C drain.
- `pathweave-test` client with deterministic payloads, SHA-256 cross-checks, download/upload/duplex/half-close modes, fixed-byte and duration runs, connection/concurrency counts, per-direction rate limiting, JSON results, Ctrl+C cancellation, and mandatory total byte budget. External addresses require `--allow-external`.
- TCP server/client local integration script and Windows TUN/Packet Monitor runner.
- Loopback runtime interface health override (`Healthy`, `Degraded`, `Unavailable`, `Recovering`) and scheduler preference/exclusion tests.
- Flow connection logs now include outbound local and peer socket endpoints for path correlation.

## Local integration results

`scripts/test-local-protocol.ps1` ran the actual test client against the separate server on `127.0.0.1`; it used a random temporary token and no external network or mobile data. Download, upload, duplex, and half-close passed deterministic pattern and SHA-256 verification. Four concurrent and fifty concurrent duplex sessions passed. A two-second duplex run, throttled to 128 KiB/s per direction, transferred 256 KiB in each direction. A duplex run with the receiver slowed to 32 KiB/s per direction completed without corruption. One thousand sequential 1 KiB upload connections completed in 5.384 seconds on loopback. The client rejected a request exceeding its total budget, and the server rejected both an over-limit request and an invalid token. All sessions shut down normally; server JSON logs record byte counts, duration, and throughput.

The latest machine-readable samples are in `target/local-protocol-results-20261125-153508`; its 1,000-session upload report records 1,000/1,000 successful sessions. These timings measure loopback only.

These loopback times and throughput are not physical-link benchmark data.

## Code verification

- 14 tests passed across the engine, test client, and test server.
- `cargo clippy --all-targets -- -D warnings` passed.
- `cargo fmt --all` passed before compilation.
- Release build and Windows live TUN integration have not yet been re-run after these changes.

## Still pending for M1A.2 acceptance

- Five-minute single and multi-connection soak through the real TUN.
- Sustained full-duplex, slow receiver/sender, cancellation under live traffic, connection churn, and live connection affinity capture.
- Live health override while multiple flows exist and simulated midstream reset/read/write failures.
- Wi-Fi-only, USB-only, and combined WAN benchmark trials against a sufficiently capable controlled VPS, with CPU/memory profiles and per-interface Packet Monitor evidence.

The Windows runner is ready, but no remote server/VPN address or WAN transfer budget has been configured. The script will require an explicit endpoint, `-AllowExternal`, and `-MaxBytes`; no external/mobile-data workload was started. See [TCP-SOAK-TESTING.md](TCP-SOAK-TESTING.md), [PERFORMANCE-BENCHMARKS.md](PERFORMANCE-BENCHMARKS.md), [FAILURE-RECOVERY.md](FAILURE-RECOVERY.md), and [REMOTE-TEST-SERVER.md](REMOTE-TEST-SERVER.md).
