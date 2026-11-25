# Pathweave M2.6 configuration

The engine reads an optional versioned TOML file with `--config <path>`. Built-in defaults are applied first, TOML values second, and explicitly supplied CLI flags last. CLI default values do not overwrite TOML values. Index-based compatibility options are resolved to the current GUID at startup; persistent interface entries use GUIDs.

```toml
[config]
schema_version = 1

[qos]
enabled = true
mode = "adaptive" # disabled, fixed, adaptive, monitor_only
aggregate_upload_mbps = 0.0
aggregate_download_mbps = 0.0
burst_ms = 50
max_flow_queue_kb = 256
max_interface_queue_mb = 16
global_memory_mb = 128
quantum_bytes = 1500
# TCP connect-establishment probe target; it is not pure RTT.
probe_target = "198.51.100.10:443"

[qos.adaptive]
target_utilization_percent = 90.0
minimum_rate_mbps = 1.0
interval_secs = 2
congestion_delay_ms = 25
recovery_delay_ms = 10
decrease_factor = 0.85
increase_percent = 5.0
minimum_samples = 3
maximum_change_percent = 15.0
cooldown_secs = 4

[qos.udp_aqm]
enabled = false
target_delay_ms = 5
interval_ms = 100

[qos.class_weights]
interactive = 8
dns = 10
best_effort = 4
bulk = 2

[qos.interfaces."01234567-89ab-cdef-0123-456789abcdef"]
enabled = true
weight = 1.0
capacity_upload_mbps = 10.0 # 0 means unknown; observed throughput is not capacity
capacity_download_mbps = 30.0
fixed_upload_mbps = 0.0
fixed_download_mbps = 0.0
minimum_upload_mbps = 0.5
minimum_download_mbps = 0.5
maximum_upload_mbps = 10.0
maximum_download_mbps = 30.0
```

Replace the example GUID with an actual value from `pathweave-engine --telemetry-once`; the example is illustrative and will not match a real adapter. To apply the configuration, select matching physical interfaces with `--interface <index,...>` or omit `--interface` to select enabled GUID entries present at startup.

```powershell
cargo run -- --validate-config --config .\Pathweave.toml
cargo run -- --config .\Pathweave.toml --interface 8,14
```

Validation is read-only and does not create a TUN device or modify routes. Unknown fields and unsupported schema versions are rejected. Queue limits must satisfy flow <= interface <= global memory. The global memory limit does not bound the vendored TCP stack's internal channels.
