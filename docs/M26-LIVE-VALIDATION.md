# M2.6 bounded live validation procedure

Live validation requires an explicit controlled server destination and a bounded byte/time budget. The existing test client supports byte-capped TCP transfers and cancellation. The M1A.2 script adds a temporary host route and Packet Monitor capture for its explicitly selected destination; inspect its parameters and require an elevated PowerShell session. Never use a default route and never disable adapters at the OS level to select a path.

Run Wi-Fi only, USB tether only, both interfaces with multiple independent flows, and a USB interruption/reconnect sequence separately. Confirm physical egress using a capture point that has first been verified on the selected adapter, then record transferred bytes, per-interface counters, queue diagnostics, process CPU, and working set. Repeat each mode with QoS disabled, fixed, and adaptive under comparable conditions. UDP AQM should use identifiable test datagrams and verify whole-datagram drops.

Every run must have a known destination, explicit maximum transfer size and duration, cancellation, and cleanup. Do not run a generic speed test or infer single-flow bonding from concurrent flow distribution.

## Validation on this host — 2026-11-25

Windows detected the Android USB tether as `Ethernet 2`, index 4, GUID `0d93791b-0078-40cb-af59-2918eed88f56`, IPv4 `10.224.251.132/24`, gateway `10.224.251.118`. Wi-Fi was index 8, GUID `23f040a9-6336-45f6-99c5-d8229fe61c22`.

A one-flow USB-only Pathweave run selected index 4, temporarily routed only `172.66.147.243/32` through the TUN, then requested `https://example.com/` with an 8 KiB range, 8 KiB/s cap, 12-second timeout, and 16 KiB response ceiling. It returned HTTP 206 with 577 bytes in 0.28 seconds. The temporary route and Pathweave TUN were removed, default routes were unchanged, and existing virtual adapters were unchanged. The result record is under `target/live-validation/`.

The Packet Monitor pipeline was tested independently of Pathweave. Ethernet 2 maps to Packet Monitor component 241 (`usb80236.sys`, Remote NDIS based Internet Sharing Device); `pktmon filter list` reported no filters. A normal TCP socket bound to interface index 4 and source `10.224.251.132` completed three handshakes to `172.66.147.243:443`. Packet Monitor started successfully for component 241, `nics`, and `all`, but each ETL contained zero packet records (three WMI header events only). Repeating with all packet types, full packet size (`--pkt-size 0`), and all event flags (`0x03f`) also produced zero packets.

The isolated Packet Monitor check is reproducible from an elevated PowerShell prompt with `scripts/windows/diagnose-pktmon-interface.ps1`; it validates the adapter GUID/MAC, refuses a pre-existing capture session, records the filter/component/session output, and sends only short ordinary TCP handshakes. It does not create routes or interact with Pathweave.

As an independent capture-point check, a short `netsh trace capture=yes` session captured 459 NDIS packet-fragment events on `MiniportIfIndex 4, LowerIfIndex 4`. Its CSV dump contains the raw outbound SYN, inbound SYN-ACK, and follow-up ACK for `10.224.251.132:12730` ↔ `172.66.147.243:443`, all tagged with interface index 4. This confirms ordinary traffic physically traversed the USB tether and that Windows NDIS packet capture can observe it. The evidence points to a Packet Monitor/Remote NDIS capture-path limitation or incompatibility on this host; the precise driver-level cause is not yet established.

This does not independently prove physical egress for the earlier Pathweave HTTPS transfer. The reusable Pathweave test script still rejects a zero-packet Packet Monitor capture; that guard is intentionally retained. Use a verified capture point or resolve Packet Monitor compatibility before treating a future Pathweave transfer as physical-egress validated. Pktmon and netsh ETL/CSV artifacts are under ignored `target/capture-diagnostics/`.

Direct interface-bound TCP handshakes to `172.66.147.243:443` and `1.1.1.1:443` succeeded from USB source `10.224.251.132`; the same probes timed out from Wi-Fi source `192.168.1.100`. A Pathweave Wi-Fi-only test, combined-interface distribution, USB interruption/reconnect, and CPU comparison remain unverified. No adapters or default routes were modified.

| Validation | Status |
| --- | --- |
| USB-bound Pathweave HTTPS transfer | Pass |
| Ordinary USB physical egress capture via NDIS/netsh | Pass |
| Packet Monitor capture on USB Remote NDIS | Fails to record packets; investigation isolated |
| Pathweave USB physical-egress capture | Pending; empty-capture rejection retained |
| Wi-Fi-only Pathweave transfer | Pending |
| Combined Wi-Fi + USB traffic | Pending |
| USB disconnect/reconnect recovery | Pending |
| CPU and memory overhead | Pending |
