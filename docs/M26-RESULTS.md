# M2.6 Results

## Implemented

- Added an independent AIMD-inspired controller per configured interface and direction. It starts only from explicitly configured capacities, honors per-interface maximums and a minimum rate, waits for persistent fresh local congestion evidence before decreasing, and increases gradually after sustained recovery.
- Connected adaptive rate changes to the existing hierarchical token buckets. Large in-flight TCP requests are paced in bounded bucket-sized chunks after a rate reduction, preserving bytes and avoiding a failed oversized token request.
- Added GUID/LUID/index fields to Windows interface telemetry, startup index-to-GUID resolution, GUID-based QoS limit syntax, periodic GUID reconciliation, changed-index rate reapplication, missing-adapter eligibility handling, and current-index probe resolution.
- Added a versioned runtime policy manager for per-interface directional rates, one-step rollback, and optional loopback-only commands. Updates apply to active flows through the shared QoS engine.
- Added opt-in CoDel-inspired UDP whole-datagram dropping based on persistent queue sojourn and backlog, with separate AQM drop diagnostics. TCP remains paced/backpressured without AQM drops.
- Added adaptive controller, identity, runtime update, AQM, and control-listener tests; added `docs/M26-ARCHITECTURE-AUDIT.md` and the M2.6 design/operations documents.

## Validation

Local unit/integration tests simulate controller congestion and recovery, stale observations, rate bounds, GUID/index changes, unknown/duplicate aliases, atomic rate application and rollback, AQM transient/persistent behavior, shaped TCP half-close, DRR fairness, and existing TCP/UDP behavior. No remote speed test or live interface benchmark is required for these tests.

Performance overhead has not been benchmarked on live Wi-Fi/USB paths. The adaptive controller and identity reconciler use one periodic Tokio task each; they sample snapshots every few seconds and do not create per-flow timers.

## Remaining M2.6 gaps

- There is no TOML file schema, migration tool, file reload, or durable policy history. The runtime API currently changes per-interface rates only; aggregate limits, mode, weights, queue/memory limits, burst, adaptive parameters, and AQM parameters remain startup settings.
- Adaptive capacity sources are explicit user values only. Historical throughput and optional controlled measurements are not implemented. Congestion control does not use loaded-vs-idle latency, jitter, or probe loss; the existing TCP connect probe is not treated as pure RTT.
- Interface operational status and GUID/index/LUID are reconciled, but address/gateway changes and adapter replacement metadata beyond GUID are not tracked. Existing sockets remain pinned and can fail if their path disappears.
- CoDel-inspired logic is not full CoDel/FQ-CoDel and currently charges token budget before a UDP AQM drop decision.
- QoS memory permits still do not cap pre-existing unbounded channels in the vendored TCP stack.
- No live Wintun, tethered WAN, throughput, or CPU-overhead benchmark was run in this implementation pass.
