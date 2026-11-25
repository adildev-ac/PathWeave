# M2 architecture and decision record

The telemetry collector and scheduler are separate. A background Tokio task calls Windows `GetIfTable2` through `spawn_blocking`, calculates rates from monotonic counter deltas, and publishes a new immutable snapshot. The selection hot path clones the snapshot pointer and reads the existing health state; it does no operating-system query or network probe.

## Scheduling decision

We chose deterministic smooth weighted round robin (SWRR) across eligible interfaces. A highest-score-only picker would strand every new flow on one adapter until the measurements changed; SWRR allocates unknown-size flows approximately in proportion to their current utility and is reproducible in tests. A flow keeps its assigned interface after selection.

The score starts with a configured interface weight. When configured internet capacity is available, OS adapter rates estimate remaining headroom and utilization. If a fresh optional probe has enough samples, the class-specific score also includes connect duration, jitter, and probe loss. Candidate scores are clamped above zero, then used as SWRR weights. Degraded and recovering paths receive reduced weights; Unavailable paths are excluded. Small score changes inside the configured hysteresis band preserve the prior score.

Capacity is never inferred from physical link speed. Capacity values are operator-provided directional internet estimates. Zero means unknown. Without configured capacity or fresh probes, selection falls back to configured weights. Shadow mode computes the exact same recommendation but routes through the round-robin baseline.

## Initial profiles

- **Bulk:** strongest weight on available configured capacity/headroom, with smaller path-quality influence.
- **Interactive and DNS:** stronger weight on latency, jitter, and loss; capacity remains a guardrail.
- **Balanced:** compromise between capacity and path quality.

These are initial heuristic coefficients, not learned or validated throughput predictions. The collected operating-system rate is shared adapter traffic and can include unrelated applications. The engine does not claim available bandwidth or a throughput improvement from these signals.
