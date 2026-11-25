# UDP session management

Each UDP session is keyed by protocol plus original source and destination socket addresses. The record carries a monotonically assigned flow ID, selected adapter index, lifecycle state, start time, byte and datagram counts in both directions, dropped datagrams, and socket errors.

The engine checks the session limit and interface health before spawning a flow. It inserts the record before opening the outbound socket, then marks the interface healthy after socket setup. A definite local path error may quarantine that adapter for the configured health cooldown. A remote service refusal or other peer-side error does not automatically label the interface unavailable.

Each accepted datagram refreshes the 120-second default idle window. Once idle, the outbound socket and flow record are released. Flow logs contain endpoints and counters, never payload contents. They include the selected interface, local outbound address, final state, duration, and termination reason.

Inbound packets are queued per UDP session with a bounded Tokio channel. When that queue fills, packet processing applies backpressure rather than growing that session queue without limit. The global TUN packet and session-accept channels in the vendored stack are still unbounded and remain a known follow-up hardening item.
