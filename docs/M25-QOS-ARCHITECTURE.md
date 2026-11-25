# M2.5 QoS Architecture

QoS is a post-selection bandwidth-sharing layer. The M2 scheduler selects the path for a new flow; that flow remains pinned to the selected interface. QoS then shapes upload and download independently for that interface and for the aggregate path.

## Data path

```text
TCP intercepted stream -> upload DRR/token bucket -> bound TCP socket
TCP bound TCP socket -> download DRR/token bucket -> intercepted stream
UDP TUN datagram -> upload DRR/token bucket -> bound UDP socket
UDP bound socket -> download DRR/token bucket -> TUN datagram
```

`src/qos.rs` owns fixed-rate token buckets, per-interface directional DRR lanes, queue permits, and QoS diagnostics. Every request is identified by the existing flow ID and classified by the existing M2 traffic rule. A lane chooses a flow using a weighted DRR quantum. Each flow's requests remain FIFO. The token bucket applies both the aggregate limit and the selected interface limit; the slower of the two required refill times gates service.

The default DRR quantum is 1,500 bytes, with initial class weights Interactive 8, DNS 10, Best Effort 4, and Bulk 2. These are service weights, not reserved rates. Unused service can be used by other active flows. This is weighted fairness rather than strict priority, so a continuously active high-class flow does not permanently exclude lower classes.

TCP shaping uses bounded relay chunks and awaits destination writes. It never discards payload bytes. The relay's read/write pairing and shutdown semantics preserve ordering, full duplex, and half-close. UDP shaping reserves capacity for complete datagrams and never splits or combines them. A UDP datagram is dropped whole if it is larger than the current permitted burst/flow size or bounded memory is unavailable. Upload and download have independent UDP futures so a paced direction does not block the other.

## Memory and measurements

The defaults cap one QoS flow request at 256 KiB, a QoS interface queue at 16 MiB per direction, and total QoS queue permits at 128 MiB. TCP requests are also capped to 16 KiB. UDP retains one datagram at a time per direction in the QoS path. Memory permits are held until the corresponding network/TUN write finishes.

Each interface/direction lane reports active flow count, waiting bytes, peak waiting bytes, shaped payload bytes, complete UDP drops, rolling average/p50/p95/p99 userspace queue delay, and token pacing wait. `--diagnostics-json` includes these values with the scheduler and adapter snapshot. Rates and byte counters refer to application payload bytes, while Windows adapter counters include all traffic on the physical adapter.

The token refill clock is monotonic. A 50 ms default burst window is configurable; the bucket has a 1,500-byte practical minimum burst to allow a small packet to progress at low configured rates. Rate limits are application payload caps, not claims about wire rate or Internet capacity.

## Scope

The first increment implements Disabled, Fixed, and MonitorOnly modes. Adaptive rate control, live policy updates, stable-GUID QoS configuration, TOML persistence, per-class max rates/reservations, CoDel, and a GUI are not implemented. QoS stays independent of WFP and does not identify executables. The vendored TCP stack still contains pre-existing unbounded internal channels, so the QoS global memory ceiling is not an end-to-end process memory ceiling. Download pacing controls local delivery and cannot remove upstream carrier queues.
