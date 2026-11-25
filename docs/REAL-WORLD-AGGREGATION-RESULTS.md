# Real-world Wi-Fi + USB aggregation demonstration

## Demonstration status

**Not demonstrated.** Pathweave currently cannot establish an outbound Wi-Fi relay connection when a destination route sends client traffic through its TUN. The relay's TCP tuple re-enters that TUN, so interface selection and successful ordinary Windows HTTPS do not constitute Pathweave egress evidence. Combined-interface throughput testing is therefore blocked at the interception/routing boundary.

This report preserves the failure as a failure. No throughput aggregation, simultaneous physical egress, or higher combined throughput is claimed.

## Existing evidence

| Check | Result |
| --- | --- |
| Direct Windows TCP/HTTPS through Wi-Fi | Passed in prior bounded diagnostic; socket bound to Wi-Fi and connected |
| Pathweave Wi-Fi-only HTTPS | Failed; timed out, zero response bytes |
| Root cause | Relay tuple was observed re-entering the Pathweave TUN under a new flow correlation |
| Wi-Fi NDIS capture | No packets matching the bounded relay destination/tuple; no physical egress proof |
| USB-only Pathweave transfer | Earlier validation exists in project history, but does not resolve this Wi-Fi route collision |
| Wi-Fi + USB simultaneous payload | Not tested; would be misleading before relay routing is fixed |
| Concurrent flow counts 2/4/8/16 | Not tested |
| Adaptive versus round-robin live comparison | Not tested |
| USB reconnect during Pathweave run | Not tested in this investigation |
| CPU/memory/throughput comparison | Not tested for equivalent live workloads |

## Current blocker and next gate

See [RELAY-ROUTING-OPTIONS.md](RELAY-ROUTING-OPTIONS.md) for the mechanism comparison. The leading supported Windows option is WFP ALE connect redirection with a callout driver and a redirect-aware user-mode proxy/engine. This is not implemented or live-validated. Do not proceed to combined-interface or USB reconnect demonstrations until a bounded single-interface test proves that the exact Pathweave relay tuple carries bidirectional payload on the intended physical NIC without re-entering TUN.

## Required evidence before claiming aggregation

For each Wi-Fi-only, USB-only, and combined run, capture dynamically resolved adapter GUID/LUID/index, source address, gateway and route; record each flow's selected interface and socket tuple; correlate that tuple with outbound and inbound NDIS payload on the matching physical interface; preserve per-flow completion and failure results; and measure comparable throughput, CPU, working set, and duration. Combined tests must use concurrent independent flows at 2, 4, 8, and 16 clients. The aggregate claim requires payload on both physical adapters and combined goodput above the best comparable single-interface run; do not infer it by summing separate single-interface results.

All capture/test scripts must remain bounded and must verify cleanup of their own process, TUN, temporary routes, and trace session. Preserve timeouts and failed captures in results.
