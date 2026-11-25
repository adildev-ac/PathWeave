# Runtime QoS Policy Updates

An optional loopback-only rate-control service can be enabled with `--qos-control-listen 127.0.0.1:48125`. It accepts at most eight concurrent clients and reads a single line capped at 1 KiB. The service rejects non-loopback bind addresses and is not started unless requested.

Commands:

```text
GET
SET-GUID <guid> <upload|download> <mbps>
SET-INDEX <index> <upload|download> <mbps>
ROLLBACK
```

Zero means unlimited. GUID settings remain associated with the configured adapter when it is absent and are applied when the same GUID returns. Index overrides resolve to the currently configured GUID and report that the index was temporary. Rate updates validate before applying, update the token buckets under one rate-state lock, increment a policy version, and retain the previous valid rate snapshot for one-step rollback. Existing sessions use the same QoS engine and sockets; no flow is moved.

Runtime control currently updates per-interface upload/download rates only. It does not reload TOML, change QoS mode, aggregate limits, class weights, memory/queue limits, burst values, adaptive-controller parameters, or AQM parameters. Fixed and monitor/disabled policies can be edited; adaptive mode owns its rates and rejects manual overrides. A file watcher and durable policy history are not implemented.
