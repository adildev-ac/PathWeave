# M2.5 QoS Implementation Results

## Implemented

- Added a dedicated QoS engine with Disabled, Fixed, and MonitorOnly modes; Fixed is opt-in and disabled remains the default.
- Added monotonic hierarchical token buckets for aggregate and per-interface upload/download payload rates.
- Added lazily created per-interface/direction weighted DRR lanes using existing flow IDs and traffic classifications.
- Added configurable class weights, quantum, burst window, per-flow/interface queue bounds, and a global QoS memory ceiling.
- Integrated TCP pacing at both relay write points while preserving full-duplex copying, byte ordering, and half-close behavior.
- Integrated whole-datagram UDP pacing with independent upload/download progress. UDP over-capacity datagrams are dropped whole and counted.
- Added lane diagnostics for queue occupancy, peak queue bytes, active flow count, bytes shaped, UDP drops, queue-delay percentiles, and pacing wait.
- Added CLI configuration and one-second JSON diagnostics. Added the architecture audit and operator documentation.

## Local validation

`cargo check --all-targets` passed.

`cargo fmt --all` completed.

`cargo test --all-targets` passed: 28 engine tests, 2 test-client tests, and 2 test-server tests.

The suite includes token-bucket pacing, DRR flow cleanup/fair service, complete UDP overload drops, existing TCP relay half-close, and bounded relay transfer checks. This validates local deterministic behavior; it is not a live multi-interface WAN throughput benchmark.

## Deferred from the larger M2.5 proposal

Adaptive QoS rate control, runtime rate changes, TOML persistence, per-class maximum rates/reservations, stable GUID configuration with adapter re-enumeration, explicit congestion alerts, per-flow QoS telemetry, and end-to-end boundedness through the vendored TCP stack remain future work. The audit documents the existing unbounded TCP-stack channels and the limits of userspace download shaping.
