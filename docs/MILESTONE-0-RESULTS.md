# Milestone 0 live TCP test

**Date:** 2026-11-25  
**Result:** Passed the initial two-link TCP path check.

## Setup

- Wi-Fi: interface 8, source IPv4 `192.168.1.100`.
- Android USB tether (Remote NDIS): interface 4, source IPv4 `10.224.251.131`.
- Prototype TUN: interface 52, test address `198.19.0.1/16`.
- Destination: `cloudflare-dns.com:443`, pinned to `1.1.1.1`; four independent HTTPS/TCP requests.
- Added a temporary `1.1.1.1/32` route through only the prototype TUN for the test.

## Results

All four requests returned HTTP 200 with `remote=1.1.1.1` and `local=198.19.0.1`, confirming they entered the prototype TUN. Packet Monitor captured the relay connections on both physical adapters:

| Physical adapter | Captured relay source | TCP connections |
| --- | --- | ---: |
| Wi-Fi (ifIndex 8) | `192.168.1.100:10299`, `192.168.1.100:10303` | 2 |
| Android USB tether (ifIndex 4) | `10.224.251.131:10301`, `10.224.251.131:10305` | 2 |

Each captured relay flow targeted `1.1.1.1:443`. This demonstrates original destination recovery sufficiently to connect to the same intercepted IP and port, plus real per-flow egress on Wi-Fi and USB tether. The two flows per adapter match the round-robin assignment.

## Cleanup and limits

The `/32` route was removed and verified absent. The prototype process was stopped, and its TUN adapter disappeared. The machine's pre-existing `net-merge` and `utun8` adapters and their routes were left in place. Wi-Fi, tether, and existing default routes remained present.

This was a short functional check against a public DNS-over-HTTPS endpoint, not a controlled throughput benchmark. It does not yet verify a sustained long-lived flow, TCP half-close behavior under load, adapter failure recovery, IPv6, UDP, or performance gains. No GUI work was started.

## Local capture files

- `target/m0-live-results.txt` — request outcomes, route lifecycle, and summarized matching packets.
- `target/m0-egress.etl` — Packet Monitor capture.
- `target/m0-egress.txt` — decoded packet capture.
