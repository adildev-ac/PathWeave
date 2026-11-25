# Milestone 1B: UDP data plane

The engine now has a datagram-oriented UDP path alongside the TCP relay. UDP remains opt-in with `--enable-udp` so the current TCP-only operating mode does not change unexpectedly.

For each original source/destination tuple, the stack preserves inbound UDP datagram boundaries. Pathweave creates one connected outbound UDP socket, applies Windows `IP_UNICAST_IF` / `IPV6_UNICAST_IF` for the selected adapter before connecting it to the recovered destination, and pins that flow to the adapter for its lifetime. Replies are written as one complete UDP packet back through the TUN stack.

The UDP path preserves empty datagrams and rejects replies that exceed the TUN MTU. It does not truncate, fragment, inspect, or decrypt DNS/QUIC payloads. Oversized requests or replies are counted as drops. The socket receive buffer is large enough for a maximum UDP payload, so a too-small userspace buffer cannot silently clip the datagram.

## Run

```powershell
cargo run -- --interface 8,14 --enable-udp
```

Use a dedicated test host route while validating the TUN engine. Do not install a default route. Confirm actual Wi-Fi and USB egress with packet captures; the selected interface log alone does not prove where Windows sent a packet.

## Limits

- M1B forwards IPv4 UDP. IPv6 UDP sessions are explicitly rejected until the IPv6 milestone.
- No interface migration occurs after the first datagram; interface selection only chooses a path for new flows.
- UDP session state expires after the configured idle timeout.
- Per-flow inbound UDP queues are bounded. The vendored IP stack still has unbounded global TUN packet and accept channels, so this milestone does not claim a process-wide memory bound under sustained overload.
- There is no retransmission or delivery guarantee, as expected for UDP.
- True single-flow bandwidth aggregation still requires a remote bonding gateway and is outside Product A.
