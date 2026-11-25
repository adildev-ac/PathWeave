# Milestone 1B: first-class UDP forwarding

## Implementation status

- Added a transport-engine boundary shared by TCP and UDP; interface selection, health state, flow records, and session capacity live in the shared context.
- Added opt-in UDP forwarding with original destination recovery, per-flow socket binding to the selected adapter, bidirectional datagram relay, idle cleanup, and per-flow telemetry.
- Added a bounded inbound queue per UDP flow and explicit session/datagram limits.
- Fixed the vendored UDP stream's byte-stream behavior for the engine path: datagram receive/send APIs preserve boundaries and empty datagrams; oversize packets are rejected rather than silently truncated.
- Added local UDP echo server/client binaries for empty, small, DNS-sized, QUIC-sized, and MTU-sized payload checks.

## Verification

`cargo check --all-targets` and `cargo build --release --bins` passed. The local loopback echo check passed 60/60 datagrams across 10 repetitions, including empty payloads and sizes 1, 12, 512, 1200, and 1472 bytes.

The loopback result verifies the UDP test utilities and OS datagram boundary behavior. Live TUN forwarding and Wi-Fi/USB adapter egress still require an elevated Windows run, a controlled destination route, and packet capture on each participating interface.

## Known limits

The vendored IP stack still uses unbounded global channels for TUN packets and accepted sessions. The new queue cap is per flow, and the session semaphore bounds spawned UDP sockets, but these changes are not a process-wide memory bound. This is recorded as a follow-up before long-duration hostile-traffic testing.

Adaptive scheduling is the next milestone after live M1B validation. This milestone still assigns new flows using the existing interface rotation and pins active sessions to their selected adapter. No remote gateway or single-flow multipath bonding was added.
