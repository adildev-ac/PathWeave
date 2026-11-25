# M2.6 bounded live validation procedure

Live validation requires an explicit controlled server destination and a bounded byte/time budget. The existing test client supports byte-capped TCP transfers and cancellation. The M1A.2 script adds a temporary host route and Packet Monitor capture for its explicitly selected destination; inspect its parameters and require an elevated PowerShell session. Never use a default route and never disable adapters at the OS level to select a path.

Run Wi-Fi only, USB tether only, both interfaces with multiple independent flows, and a USB interruption/reconnect sequence separately. Confirm physical egress in Packet Monitor using fresh adapter indexes, then record transferred bytes, per-interface counters, queue diagnostics, process CPU, and working set. Repeat each mode with QoS disabled, fixed, and adaptive under comparable conditions. UDP AQM should use identifiable test datagrams and verify whole-datagram drops.

Every run must have a known destination, explicit maximum transfer size and duration, cancellation, and cleanup. Do not run a generic speed test or infer single-flow bonding from concurrent flow distribution.

## Validation on this host — 2026-11-25

Windows detected the Android USB tether as `Ethernet 2`, index 4, GUID `0d93791b-0078-40cb-af59-2918eed88f56`, IPv4 `10.224.251.132/24`, gateway `10.224.251.118`. Wi-Fi was index 8, GUID `23f040a9-6336-45f6-99c5-d8229fe61c22`.

A one-flow USB-only Pathweave run selected index 4, temporarily routed only `172.66.147.243/32` through the TUN, then requested `https://example.com/` with an 8 KiB range, 8 KiB/s cap, 12-second timeout, and 16 KiB response ceiling. It returned HTTP 206 with 577 bytes in 0.28 seconds. The temporary route and Pathweave TUN were removed, default routes were unchanged, and existing virtual adapters were unchanged. The result record is under `target/live-validation/`.

Packet Monitor produced an ETL with zero packet records on both the generic NIC selection and the tether's resolved component ID 241. Therefore the HTTPS result verifies a successful one-interface Pathweave data-plane transfer over the selected USB-bound socket, but physical NIC egress was not independently proven by packet capture. The reusable test script now rejects a zero-packet capture instead of reporting a full pass.

Direct interface-bound TCP handshakes to `172.66.147.243:443` and `1.1.1.1:443` succeeded from USB source `10.224.251.132`; the same probes timed out from Wi-Fi source `192.168.1.100`. A Pathweave Wi-Fi-only test, combined-interface distribution, USB interruption/reconnect, and CPU comparison remain unverified. No adapters or default routes were modified.
