# UDP Active Queue Management

Optional UDP AQM is enabled with `--qos-udp-aqm`. Its defaults are a 5 ms target and 100 ms persistence/control interval; use `--qos-udp-aqm-target-ms` and `--qos-udp-aqm-interval-ms` to change them. The per-flow DRR lane stores enqueue instants and CoDel-inspired persistent-delay state. A datagram is eligible for a drop only when its sojourn remains above target for the interval and at least an MTU-sized backlog remains. When active, drop spacing decreases approximately with `interval / sqrt(drop_count)`. Recovery below target exits drop state.

The implementation drops only whole UDP datagrams and reports `udp_aqm_drops` separately from total UDP drops. Remaining datagrams retain their FIFO order. TCP does not pass through this AQM logic and no TCP bytes are discarded.

This is CoDel-inspired application-queue management, not RFC 8289 conformance and not RFC 8290 FQ-CoDel. The queue is a userspace DRR admission/service queue, not the physical interface's packet queue. The current implementation spends token budget before it can decide to AQM-drop an already paced datagram. It cannot remove delay already accumulated in the OS, carrier, or Internet path.

References: [RFC 8289](https://www.rfc-editor.org/rfc/rfc8289.html) defines persistent-sojourn estimation and control; [RFC 8290](https://www.rfc-editor.org/rfc/rfc8290.html) specifies FQ-CoDel packet scheduling.
