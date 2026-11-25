# Scheduler algorithm

## Candidate utility

Each eligible interface starts with its configured base weight (default 100, allowed range 1–1000). If a user-configured directional capacity exists, fresh adapter counters provide directional utilization and estimated headroom. For Bulk, the headroom and utilization terms receive most of the capacity weight. Interactive/DNS weight latency, jitter, and recent probe loss more heavily. Balanced uses intermediate weights. Probe quality contributes only after at least three recent samples and while the last successful sample is fresh.

Latency quality is normalized as `1 / (1 + latency_ms / 100)`, jitter as `1 / (1 + jitter_ms / 50)`, and loss as `1 - loss_ratio`; each remains within [0,1]. Unknown terms are omitted through neutral fallback values. Configured capacity has lower confidence when adapter counters are missing or stale. OS counter rates are not represented as application-only load.

Per-interface utility is multiplied by the base weight. Static `weighted` mode uses only base weights. `adaptive` uses computed weights; when all measurement confidence is below 0.25, it reports fallback and uses configured weights. `adaptive-shadow` logs candidate score and reason while assigning by round-robin.

## Distribution and stability

Smooth weighted round robin adds each candidate's weight to its current accumulator, chooses the largest accumulator, then subtracts the total weight from that candidate. This yields deterministic proportional assignment across a sequence of new flows and works with unknown connection sizes. Score changes smaller than the configured 15% hysteresis band retain the previous score. Existing flows never migrate.

The model is intentionally small and explainable. It does not estimate queue occupancy, application demand, or true available bandwidth. Capacity values are operator supplied; active bandwidth tests are absent. The probe is TCP connection establishment, not ICMP/UDP RTT.
