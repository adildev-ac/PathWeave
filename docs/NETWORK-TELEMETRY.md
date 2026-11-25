# Network telemetry

On Windows, one background collector samples `GetIfTable2` once per second. It records stable adapter GUID, current index, operational status, MTU, media transmit/receive link speeds, cumulative octets, errors, and discards. Link speeds are diagnostic values only and are never used as internet-capacity estimates.

Run `pathweave-engine --telemetry-once` for a read-only one-time JSON snapshot, without creating the TUN device.

Download/upload rates are counter deltas divided by monotonic elapsed seconds and smoothed with EMA (default alpha 0.35, configurable by CLI). Counter decreases reset the rate baseline. A GUID observed under a new index creates a fresh baseline and the GUID remains visible in diagnostics. OS rates include unrelated host traffic. The existing per-flow byte totals separately represent application traffic handled by this process.

An optional `--probe-target IP:PORT` performs a TCP connection-establishment probe on each configured interface (default two-second interval, 1500 ms timeout). Successful samples feed EMA latency, minimum latency, and absolute-difference EMA jitter. The last 60 attempts form a loss ratio. A failed target is telemetry only: it never marks the adapter unavailable. This is one configurable endpoint, so endpoint failure can still bias the metric; omit the option when no suitable responsive endpoint is available.

Diagnostics JSON reports counter and probe freshness. Stale or missing metrics are ignored by scoring. The current implementation does not use a blocking call on the async executor. Adapter identity is collected, but operators still configure interface indexes and the socket binder does not yet re-resolve by GUID.
