# M2.6 adaptive telemetry semantics

Adaptive capacity is trusted only when explicitly configured per GUID and direction. A zero capacity means unknown; adaptive shaping for that direction remains inactive. No physical Wi-Fi/USB link rate or observed adapter throughput is promoted to an internet capacity estimate.

The Windows counter collector stores a bounded rolling history of the latest 60 upload/download observations for each interface GUID, with timestamps. These samples measure all host traffic on that adapter, not just Pathweave flows. They are diagnostic history only and are not used as a capacity ceiling.

The adaptive controller currently uses fresh local QoS queue sojourn and Windows interface error/discard ratios. It requires repeated samples and configured confidence thresholds; stale data does not change the effective rate. Existing probes measure TCP connection establishment duration, which includes endpoint and handshake processing and is not labeled or consumed as pure RTT. A probe failure is not treated as a network packet-loss sample for adaptive QoS. Baseline/loaded latency and controlled capacity measurement are not implemented.
