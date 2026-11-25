# Milestone 0: TUN TCP feasibility

Live hardware results: [MILESTONE-0-RESULTS.md](MILESTONE-0-RESULTS.md).

## Demonstration target

On Windows 10/11, route only a controlled test destination through a Wintun-backed TUN. Accept the client's real TCP connection in userspace, record the flow's source and original destination addresses and ports, and relay it through a socket pinned to a chosen physical adapter. Repeat concurrent connections with Wi-Fi and Android USB tethering selected and verify their egress adapters with packet capture.

This milestone demonstrates connection-level distribution. A single TCP flow stays on one adapter and is not bonded across links.

## Current prototype

`src/main.rs` uses the published `ipstack` async TUN stack. Its TCP stream exposes `peer_addr()` (the intercepted destination) and `local_addr()` (the originating client endpoint). The relay selects an interface index round-robin and applies Windows `IP_UNICAST_IF` or `IPV6_UNICAST_IF` to the outbound socket before connecting. The TUN address is in the benchmarking-only `198.18.0.0/15` range.

This is a feasibility harness, not yet a safe system-wide traffic service. It has no route installer, route rollback, DNS interception, UDP relay, adapter health monitor, or crash recovery. Keep traffic capture limited to a controlled test route until those pieces exist.

The vendored `ipstack` 1.0.1 source differs from the upstream package only in `build.rs`: it now creates the `target/<profile>/examples` directory before copying the Wintun DLL. Without that directory, the upstream Windows build script fails on a fresh Cargo target directory.

## Stack decision: provisional

Use `ipstack` 1.0.1 for the first spike because it explicitly targets async TUN use and its accepted TCP stream exposes both endpoints, which directly answers the original-destination question. The crate is Apache-2.0 and documents itself as unstable and under development. Do not treat it as a production stack until the acceptance matrix below passes. Compare against smoltcp after the first live run; smoltcp is mature and modular but its explicit socket lifecycle requires more integration work for arbitrary transparent flows.

## Acceptance checklist

- [ ] Build and run as administrator with Wintun installed from its official distribution.
- [ ] Configure one test destination route through the TUN and prove no default route was changed.
- [ ] Establish TCP connections to two controlled IPv4 endpoints; log the exact original destination IP and port for each.
- [ ] Repeat with IPv6 after confirming the selected physical adapters have working IPv6 routes.
- [ ] Verify the selected egress adapter from a capture on each adapter, not only application logs.
- [ ] Open many independent connections and observe connections on Wi-Fi and USB tether simultaneously.
- [ ] Verify a single long-lived TCP transfer uses only its selected adapter.
- [ ] Exercise orderly close and one-sided shutdown; check relay byte counts and clean teardown.
- [ ] Disconnect one adapter during active and new connections; ensure new flows can be assigned to the remaining adapter.
- [ ] Measure one-flow and parallel-flow throughput against direct networking baselines.

## Known constraints

- `ipstack` is an early, unstable dependency; confirm fragmentation, TCP half-close, concurrency, IPv6, and memory behavior experimentally.
- Wintun and administrator privileges are required on Windows. Do not ship a driver binary until its source, signature, and distribution terms are reviewed.
- The bundled Wintun DLL has a valid Authenticode signature from WireGuard LLC in this workspace; this verifies the build-time binary only, not an installed/running TUN adapter.
- Interface indexes are supplied manually for this spike. Discovery and stable GUID mapping belong in a later engine layer.
- The current scheduler is round-robin and intentionally ignores measured link capacity.
- UDP is observed but not forwarded in this TCP-only milestone.


