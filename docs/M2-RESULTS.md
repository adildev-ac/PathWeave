# Milestone 2 results

## Implemented

- Kept round-robin as the default and added static weighted, adaptive, and adaptive-shadow scheduling modes.
- Integrated the scheduler through the shared transport boundary. TCP and UDP receive a protocol-independent flow description, one decision is made per new flow, and existing interface affinity remains unchanged.
- Added smooth weighted round robin over explainable candidate scores, configurable interface weights and directional capacity, metric freshness checks, score hysteresis, health penalties, fallback, and per-flow shadow explanations.
- Added first-match traffic rules by transport, destination port, or IPv4/IPv6 CIDR. DNS port 53 defaults to DNS; other traffic including UDP 443 defaults to Balanced. No payload inspection or application identity is used.
- Added Windows `GetIfTable2` adapter telemetry: stable GUID, alias, current index, operational state, MTU, counters, errors/discards, and media link speeds. Counter rates use monotonic deltas and EMA. Adapter rates are separate from application-level flow counters. `--telemetry-once` provides a read-only JSON snapshot without creating a TUN.
- Added optional per-interface TCP connect-duration probes with EMA latency, minimum latency, jitter, recent loss, success/failure counts, and freshness. Failed probes do not quarantine an adapter.
- Added JSON telemetry diagnostics and deterministic scheduler simulations.

## Verification

- `cargo test --all-targets`: 29 passed, 0 failed (25 engine tests plus the existing test-binary tests).
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo check --target x86_64-pc-windows-msvc --all-targets`: passed.
- `cargo build --release`: passed.
- `--telemetry-once` ran successfully on this Windows host and returned adapter names, GUIDs, indexes, status, counters, and link data from `GetIfTable2`.
- Release scheduler-only simulation, using two eligible interfaces and excluding operating-system and network I/O:

| Decisions | Average | p95 | p99 |
| ---: | ---: | ---: | ---: |
| 1,000 | 1.29 µs | 1.40 µs | 2.20 µs |
| 10,000 | 0.93 µs | 1.50 µs | 1.60 µs |
| 100,000 | 0.85 µs | 1.00 µs | 1.30 µs |

These are local scheduler decision timings, not network throughput claims. CPU and process-memory measurements were not captured.

## Limitations / next work

- M2 remains opt-in and has not been validated with live TUN traffic over Wi-Fi plus Android USB tethering. Shadow mode is the recommended first live run.
- Interface indexes are still configured by the operator. GUIDs are collected for identity diagnostics, but socket creation does not yet re-resolve an index from the GUID after adapter replacement.
- Capacity is operator configured; no automatic internet-capacity estimator or active bandwidth test is used. Physical link speed is never used as capacity.
- Only one optional TCP probe target is supported. Its measurement includes endpoint behavior and connection establishment. Multiple independent targets, ICMP/UDP probes, rolling median/p95, and corroborated path health are not implemented.
- Error/discard ratio and probe metrics are heuristic indicators; no traffic shaping, WFP, route management, per-application identity, or true multipath bonding is included.
- CLI configuration is available; persisted TOML and live control subcommands are not implemented.

Before enabling `adaptive`, run `adaptive-shadow` with representative traffic and confirm fresh counters, realistic capacity estimates, and adapter egress through packet capture. The next milestone should be live shadow validation and interface identity re-resolution before adaptive routing becomes a recommended default.
