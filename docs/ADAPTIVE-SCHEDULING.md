# Adaptive scheduling

The scheduler affects only new TCP connections and UDP sessions. Round-robin remains the default. Choose `--scheduler-mode weighted`, `adaptive`, or `adaptive-shadow` explicitly to change behavior. Each selected flow stays pinned to its original interface.

`weighted` uses operator-set base weights. `adaptive` adjusts those weights using configured directional internet capacity, fresh Windows adapter counters, optional TCP connect-duration latency/jitter/loss probes, recent error/discard ratio, and interface health. The scheduler uses deterministic smooth weighted round robin, so allocations follow relative utility across new flows rather than routing every flow to one momentary winner. When telemetry is stale or unavailable, it falls back to the configured weights and marks the decision.

`adaptive-shadow` computes and logs the adaptive recommendation, candidate scores, confidence, class, and reason, but the actual interface remains the existing round-robin choice. A per-flow decision is logged once at flow creation. `--diagnostics-json` publishes current telemetry and scheduler candidates once per second to stdout.

Example:

```powershell
cargo run -- --interface 8,14 --scheduler-mode adaptive-shadow `
  --interface-weight 8=100 --interface-weight 14=100 `
  --interface-capacity-mbps 8=100:20 --interface-capacity-mbps 14=50:15 `
  --traffic-rule tcp:203.0.113.0/24=bulk `
  --diagnostics-json
```

Add `--probe-target IP:PORT` only when you have a controlled endpoint that accepts TCP connections over both paths.

Capacity syntax is `INDEX=DOWNLOAD_Mbps:UPLOAD_Mbps`; zero means unknown. A link/media speed from Windows is shown for diagnostics and never treated as internet capacity. Probe results are TCP connection-establishment durations, not pure RTT. Probe failures do not quarantine an interface.

All tunables are CLI options: telemetry interval/EMA alpha, maximum metric age, scheduler hysteresis, minimum probe samples, probe interval/timeout, interface base weights, capacities, traffic rules, and strategy. There is not yet a persisted TOML config file or interactive `scheduler set` subcommand.
