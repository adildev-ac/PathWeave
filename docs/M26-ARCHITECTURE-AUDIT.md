# M2.6 Architecture Audit

## Existing implementation

- `src/tcp_transport.rs` selects an interface once per new flow, opens a bound socket, and runs two directional relay copies. `src/relay.rs` has the fixed QoS write points; its fixed-mode queue request is limited to 16 KiB and TCP never intentionally drops bytes.
- `src/udp.rs` selects an interface once per five-tuple. Shaped UDP uses independent receive/send halves and waits for whole-datagram QoS permits before socket/TUN sends. The vendored IP stack's UDP ingress queue is bounded, while its packet output channel is unbounded.
- `src/qos.rs` owns a fixed `QosConfig`, per-interface/direction DRR workers, aggregate and interface token buckets, semaphore-backed queue memory, and a 256-sample lane sojourn window. The token bucket reads rates from its construction-time configuration; there is no runtime update path.
- `src/scheduler.rs` remains independent. It asks `InterfacePool` for current numeric indexes and uses configured user capacities plus interface counters/probes to select new flows. Existing flow sockets are not migrated.
- `src/telemetry.rs` already calls Windows `GetIfTable2`; rows include interface GUID, current index, alias, operational status and physical link speeds. It does not currently retain the row's `InterfaceLuid` value. Link speed is diagnostic only, not Internet capacity.
- `src/interface.rs` stores a static vector keyed by numeric index. Health state, socket binding, scheduler configuration and QoS limits therefore become stale when Windows assigns a different index.
- `src/main.rs` configures everything from CLI flags. There is no persistent TOML configuration, reload path, runtime policy manager or local QoS control service.
- The vendored TCP stack includes unbounded channels for packet/stream data. QoS memory permits bound QoS requests, not every process allocation.

## Design for M2.6

Use only explicit user capacities as adaptive controller capacity input; traffic counters and physical link speeds are observations, not capacity. Run independent upload/download controllers per stable adapter identity. The controller uses lane queue delay and interface error/discard metrics, requires repeated fresh evidence, decreases conservatively, and ramps back up slowly. If capacity is unknown, leave that direction's adaptive shaper inactive unless the user supplied a maximum/fixed rate.

The Windows adapter table provides GUID, LUID and current index in one snapshot. Resolve configured indexes to GUID/LUID at startup, then reconcile configured identities from later snapshots. Never map an unknown adapter by alias. Keep established sockets pinned to their original numeric binding and report path loss; refreshed indexes apply to new flows.

Add a validated, versioned policy manager and an optional loopback-only control surface. Existing active QoS sessions must consult the same mutable engine so a rate update affects them without replacing sockets. Queue/memory limit changes must preserve in-flight permits and enforce the new ceiling for future admissions.

CoDel-inspired UDP management is opt-in and operates only on complete datagrams admitted to a userspace lane. It uses per-flow persistent-sojourn state and complete datagram drops. It is not full CoDel/FQ-CoDel: the relay boundary is not an IP packet scheduler, and its sojourn excludes queues already in the OS/carrier path.
