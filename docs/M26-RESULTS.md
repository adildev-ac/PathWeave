# Pathweave M2.6 closure status

## Implemented

- Independent AIMD-inspired upload/download controllers per configured interface. Adaptation requires explicit per-direction capacity, honors configured min/max values, responds to repeated fresh local queue/error evidence, and ramps recovery gradually.
- Runtime rates use the existing token buckets. Large in-flight TCP requests are split into paced reservations after a rate decrease, so queued bytes are preserved.
- Version 1 TOML QoS schema with whole-file validation, GUID-keyed interface policy, startup precedence (defaults < TOML < explicit CLI), a read-only `--validate-config` command, and explicit v0 migration with backup and atomic replacement.
- Bounded last-60 observed throughput samples with timestamps, kept distinct from trusted configured capacity. A low observed rate is never used as a link capacity estimate.
- Windows polling of `GetIfTable2`, unicast address tables, and default routes refreshes GUID/LUID/index, addresses, operational state, and default next hops. Changes apply to new-flow selection, probes, and rates; established flows remain pinned.
- Versioned per-interface rate updates and rollback. Optional loopback control requires a 32-byte token for mutations, with request bounds and timeout.
- Optional CoDel-inspired UDP whole-datagram AQM. TCP is never dropped by the UDP AQM path.
- Product/CLI branding changed to **Pathweave**. Added an MIT license and configuration, migration, runtime-control, telemetry, reconciliation, and live-validation docs.

## Validation performed

- Host tests cover adaptive stability/recovery, unknown capacity, TOML validation/migration, CLI-over-TOML precedence, throughput-history bounds, address/gateway refresh semantics, GUID/index reconciliation, atomic rate updates/rollback, UDP AQM, DRR, and TCP/UDP regressions.
- `cargo test --workspace`: 50 engine tests and 4 client/server protocol tests passed. `cargo fmt --all -- --check`, Clippy with `-D warnings`, Windows-target check, and release build passed.
- The bounded local protocol script passed 12 checks, including TCP download/upload/duplex/half-close, concurrent flows, pacing, byte caps, and authentication rejection.
- `--telemetry-once` ran on this Windows host and returned populated Wi-Fi addresses and default gateway from the native IP Helper tables.
- Current live validation: Wi-Fi is active at index 8 and Android USB tether is active as Ethernet 2 at index 4. A USB-bound Pathweave HTTPS request passed. Packet Monitor records zero packets from the tether despite a verified component mapping and clean filters; independent `netsh trace` captured the ordinary USB-bound TCP handshake on NDIS interface 4. See [M2.6 live validation](M26-LIVE-VALIDATION.md) for the isolated capture findings. Wi-Fi-only Pathweave, combined-path distribution, interruption/reconnect, and CPU comparisons remain unverified.

## Still open

- Full transactional TOML runtime reload is not implemented. The runtime API changes per-interface rates only; mode, aggregate rates, weights, queue bounds, adaptive settings, AQM, and probes are startup settings.
- Native IP Helper change notifications are not wired; reconciliation polls fresh tables. Startup does not yet retain configured-but-absent GUID entries as offline slots.
- Probe duration remains TCP connect-establishment time, not pure RTT. There is no baseline/loaded latency signal, active capacity calibration, or throughput-based capacity learning.
- The CoDel-inspired UDP path is not full CoDel/FQ-CoDel and currently consumes token budget before its AQM drop decision.
- QoS memory permits do not bound pre-existing unbounded internal channels in the vendored TCP stack.
- Live WAN throughput and CPU/memory overhead results remain unmeasured. These gaps prevent marking M2.6 fully closed.

## Next milestone

Finish full runtime policy reload and offline adapter startup/recovery, then run bounded Wi-Fi and USB tether validation with Packet Monitor and a controlled endpoint. Begin M3/WFP only after those M2.6 closure criteria are met.
