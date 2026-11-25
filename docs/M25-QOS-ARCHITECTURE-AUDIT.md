# M2.5 QoS Architecture Audit

## Existing data paths

`src/tcp_transport.rs` accepts an intercepted TCP stream, asks the existing M2 scheduler to select an interface for the new flow, binds a physical `TcpStream` to that interface, and invokes `src/relay.rs`. The relay splits both streams and runs two copies concurrently. Each direction shuts down its destination writer when its source reaches EOF, preserving TCP half-close. Tokio copy/write backpressure prevents the relay from continuing to read while its destination is blocked.

`src/udp.rs` keeps one session per five-tuple and one interface-bound `UdpSocket` per session. Its original loop selects between TUN datagrams and socket replies and forwards each datagram without combining or splitting it. The vendored `IpStackUdpStream` preserves empty datagrams and its inbound channel has configurable bounded capacity (default 8).

## Existing buffers and queueing

- The TCP relay uses Tokio's bounded copy buffer. The shaped relay uses a fixed 16 KiB stack buffer and writes each permitted segment before reading more.
- The vendored TCP TCB defaults to a 16 KiB read buffer.
- The vendored TCP implementation also has unbounded Tokio channels for accepted streams, packet delivery, and stream data. These are pre-existing stack queues outside this QoS layer and need a separate stack-level bounded-channel review before claiming a strict end-to-end memory ceiling.
- UDP has a bounded per-flow inbound packet channel; the application receive buffer is 65,535 bytes. QoS holds one complete datagram per active direction while it waits for service.
- The application tracks flow counts with semaphores and flow records with `FlowManager`. It already tracks TCP byte counters and UDP datagram/byte/drop counters.

## Interface and classification boundary

The M2 scheduler chooses an interface only when a new flow is accepted. Each flow retains that interface for its lifetime. It also classifies a flow from the configured protocol, destination port, and destination CIDR rules. QoS receives that class and the selected interface index; it does not choose or migrate paths and does not inspect packet contents or process identity.

Windows adapter telemetry already contains a GUID and current index. `InterfacePool` and socket binding are currently keyed by the configured interface index, however; QoS per-interface CLI limits use the same index. Stable GUID-based persistence, periodic re-enumeration, and rebinding remain unimplemented. Established sockets are not migrated.

## QoS integration points

- TCP upload is paced after reading from the intercepted stream and before writing to the physical socket.
- TCP download is paced after reading from the physical socket and before writing to the intercepted stream.
- UDP upload waits for a whole-datagram permit before `UdpSocket::send`.
- UDP download waits for a whole-datagram permit before returning the response through the TUN stream.
- The UDP stream is split into receive and send handles in the vendored crate so an upload wait cannot prevent a download response from progressing.

The QoS engine has per-interface/direction DRR lanes, a hierarchical aggregate-plus-interface token bucket, per-flow queue records, semaphore-backed global/interface memory permits, and rolling lane queue-delay samples. Locks are held only for short queue/config/stat operations and never across socket I/O.

## Current behavior and known gaps

- QoS is disabled by default. `fixed` enforces configured token-bucket rates. `monitor-only` runs DRR and records queue telemetry without rate pacing. There is no adaptive QoS mode or live runtime rate-update API yet.
- TCP never drops payload. Pacing blocks the relay direction, and writes use the existing async backpressure path. UDP drops only complete datagrams when they exceed the configured burst/flow bound or cannot immediately obtain bounded queue memory; each drop is counted.
- QoS queue-delay samples include the time from lane admission through token pacing until service. They describe userspace shaping delay, not carrier queue delay.
- TCP's underlying vendored stack channels are unbounded, so `global_memory_limit_bytes` bounds QoS permits, not all memory used by the complete application.
- Queue delay and lane byte/drop counters are exposed in JSON diagnostics. The current metrics are lane-level; per-flow queue telemetry, explicit congestion alerts, retransmission signals, and backpressure-duration totals are future work.
- Adapter link speed is diagnostic only. The QoS engine does not infer internet capacity from it.
- Download shaping controls delivery into the TUN/application and userspace memory use. It cannot remove queues already formed upstream in the carrier or Internet path.
- No CoDel, packet dropping for TCP, GUI, WFP, TOML configuration, adaptive rate control, or interface migration is part of this fixed-shaping increment.
