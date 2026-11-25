# M2.5 QoS Configuration

QoS is off by default. Configure it through CLI flags; this increment does not add a TOML file or live reconfiguration interface. The existing M2 scheduler continues to select each new flow's interface. QoS rate limits use application payload bytes and Mbps input values.

## Fixed shaping example

```powershell
pathweave-engine.exe --interface 12,34 --enable-udp `
  --qos-mode fixed `
  --qos-upload-mbps 25 --qos-download-mbps 100 `
  --qos-interface-limit 12=15:70 `
  --qos-interface-limit 34=10:30 `
  --qos-burst-ms 50
```

The per-interface syntax is `INDEX=UPLOAD:DOWNLOAD`. Zero means unlimited for that direction. Aggregate and per-interface limits both apply; a payload is charged once to each applicable bucket. Interface indexes must also appear in `--interface`.

## Fairness and memory controls

```powershell
--qos-class-weight interactive=8 `
--qos-class-weight dns=10 `
--qos-class-weight best-effort=4 `
--qos-class-weight bulk=2 `
--qos-quantum-bytes 1500 `
--qos-max-flow-queue-kb 256 `
--qos-max-interface-queue-mb 16 `
--qos-global-memory-mb 128
```

Class weights must be 1 through 100. Queues and memory must be positive values within Tokio semaphore limits. Fixed mode requires at least one nonzero aggregate or interface limit. Monitor-only mode runs DRR and reports lane telemetry without enforcing a rate. Disabled mode bypasses QoS. There is no adaptive QoS mode yet; M2 adaptive scheduling remains available independently through `--scheduler-mode`.

Use `--diagnostics-json` to emit a one-second JSON snapshot. QoS output includes current QoS memory permits and per-interface/direction lane queue, delay, byte, and UDP drop statistics. These are userspace measurements, not physical link capacity estimates.

## Policies and limitations

TCP is paced and backpressured; no TCP application bytes are dropped. UDP is kept datagram-atomic. A datagram that cannot fit the configured burst or bounded queue is dropped whole and counted. An overload at the pre-existing bounded IP-stack UDP ingress queue can also discard a datagram before it reaches QoS.

Per-interface settings currently key on the live Windows interface index. Index persistence across adapter disable/enable is not guaranteed. Stable GUID configuration and live index reconciliation are future work. Existing sockets remain on their original paths.
