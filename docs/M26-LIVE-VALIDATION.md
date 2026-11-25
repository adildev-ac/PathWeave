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

The bounded Pathweave test script supports an opt-in `-CaptureBackend NetshTrace` mode while retaining Packet Monitor as its default and retaining the zero-packet rejection. The NDIS backend records the live interface identity, Pathweave's actual outbound socket endpoints, and validates both directions of the exact TCP tuple from decoded ETL packet bytes. It requires captured inbound TCP payload to cover the HTTP response body; a handshake-only trace is rejected. The same script now accepts an active Wi-Fi or Ethernet adapter, provided its current GUID matches the supplied value.

Examples from an elevated PowerShell prompt:

```powershell
.\scripts\windows\test-pathweave-interface.ps1 -InterfaceIndex 8 -ExpectedGuid '23f040a9-6336-45f6-99c5-d8229fe61c22' -DestinationIPv4 '172.66.147.243' -HostName 'example.com' -CaptureBackend NetshTrace
.\scripts\windows\test-pathweave-multiflow.ps1 -InterfaceIndex @(8, 4) -ExpectedGuid @('23f040a9-6336-45f6-99c5-d8229fe61c22', '0d93791b-0078-40cb-af59-2918eed88f56') -DestinationIPv4 '172.66.147.243' -HostName 'example.com' -FlowCount 4
```

An Ethernet 2 Pathweave run returned HTTP 206 with a 577-byte body. Pathweave logged physical socket `10.224.251.132:4740` → `172.66.147.243:443`, interface index 4. The independent NDIS ETL matched 9 outbound and 10 inbound packets on interface 4, with 680 outbound and 5,751 inbound TCP payload bytes. Route and TUN cleanup passed, default routes were unchanged, and pre-existing virtual adapters were unchanged. The result record is under `target/live-validation/`.

Direct interface-bound TCP handshakes to `172.66.147.243:443` and `1.1.1.1:443` succeeded from USB source `10.224.251.132`; the same probes timed out from Wi-Fi source `192.168.1.100`. A Pathweave Wi-Fi-only test, combined-interface distribution, USB interruption/reconnect, and CPU comparison remain unverified. No adapters or default routes were modified.

The Wi-Fi-only Pathweave test was attempted with interface index 8 and the NDIS backend. A direct Wi-Fi-bound curl probe timed out after 4 seconds; the bounded Pathweave HTTPS request timed out after 5 seconds. The trace recorded 231 NDIS events on Wi-Fi, but none contained the test destination address. The adapter was up, but no usable connection to the controlled endpoint was observed over Wi-Fi.

Four HTTPS clients were then launched concurrently with Pathweave configured for Wi-Fi index 8 and USB index 4. One client completed HTTP 206 with 577 bytes; three clients timed out. Scheduler logs show four flows assigned to each interface, but Pathweave logged eight total flows while only four clients were launched, so only the successful client could be confidently correlated by its TUN-side source port. That flow used USB socket `10.224.251.132:8753` → `172.66.147.243:443` and carried 680 outbound plus 5,751 inbound TCP payload bytes. The other three observed USB sockets had no TCP payload, and the four Wi-Fi-selected flows timed out during connect. The NDIS trace contained no frame with the test destination on Wi-Fi. This is incomplete evidence for concurrent distribution and does not demonstrate successful Wi-Fi egress or two-link aggregation. The harness failed while assembling its final JSON after capture cleanup; the retained ETL and logs were decoded separately and the result is saved under `target/live-validation/`.

| Validation | Status |
| --- | --- |
| USB-bound Pathweave HTTPS transfer | Pass |
| Ordinary USB physical egress capture via NDIS/netsh | Pass |
| Packet Monitor capture on USB Remote NDIS | Fails to record packets; investigation isolated |
| Pathweave USB physical-egress capture via NDIS/netsh | Pass; 577-byte HTTP body matched to USB socket payload |
| Wi-Fi-only Pathweave transfer | Failed to reach the controlled endpoint; investigate Wi-Fi path |
| Combined Wi-Fi + USB traffic | Incomplete; one of four clients completed, USB carried the verified payload |
| USB disconnect/reconnect recovery | Pending |
| CPU and memory overhead | Pending |
