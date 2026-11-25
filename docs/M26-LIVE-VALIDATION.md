# M2.6 bounded live validation procedure

Live validation requires an explicit controlled server destination and a bounded byte/time budget. The existing test client supports byte-capped TCP transfers and cancellation. The M1A.2 script adds a temporary host route and Packet Monitor capture for its explicitly selected destination; inspect its parameters and require an elevated PowerShell session. Never use a default route and never disable adapters at the OS level to select a path.

Run Wi-Fi only, USB tether only, both interfaces with multiple independent flows, and a USB interruption/reconnect sequence separately. Confirm physical egress in Packet Monitor using fresh adapter indexes, then record transferred bytes, per-interface counters, queue diagnostics, process CPU, and working set. Repeat each mode with QoS disabled, fixed, and adaptive under comparable conditions. UDP AQM should use identifiable test datagrams and verify whole-datagram drops.

Every run must have a known destination, explicit maximum transfer size and duration, cancellation, and cleanup. Do not run a generic speed test or infer single-flow bonding from concurrent flow distribution.

## Validation on this host

The current adapter inventory has active Wi-Fi at index 8 and an Ethernet device with no usable default route; it does not contain an Android USB tether/Remote NDIS adapter. No controlled remote test endpoint was supplied in this closure request. Therefore physical Wi-Fi/USB egress capture, combined-path distribution, disconnect/reconnect, and CPU comparison were not run. Do not treat unit tests or interface table snapshots as live traffic validation.
