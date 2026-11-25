# Milestone 1A live TCP validation

**Date:** 2026-11-25  
**Result:** Passed the targeted, concurrent two-interface TCP forwarding check.

## Test setup

- TUN: `NetmergePrototype`, interface 47, `198.19.0.1/16`.
- Wi-Fi: interface 8, source IPv4 `192.168.1.100`.
- Android USB tether (Remote NDIS): interface 4, source IPv4 `10.224.251.131`.
- Destination: `1.1.1.1:443` (`cloudflare-dns.com`), pinned by curl `--resolve`.
- Eight parallel, independent HTTPS/TCP requests through a temporary `1.1.1.1/32` route to the prototype TUN.
- Packet Monitor captured NIC traffic with 96-byte packet prefixes.

## Results

All eight requests completed with exit code 0 and HTTP 200. Each reported `remote=1.1.1.1` and `local=198.19.0.1`, showing that the client connections entered the TUN. All eight requests completed in 2,268 ms wall-clock time (individual curl times were 76–546 ms).

The NIC capture contains eight distinct outbound TCP source endpoints to `1.1.1.1:443` across both configured physical adapters:

| Physical adapter | Captured source endpoints | Flows |
| --- | --- | ---: |
| Wi-Fi (`ifIndex 8`) | `192.168.1.100:5738`, `:5746`, `:5753` | 3 |
| Android USB tether (`ifIndex 4`) | `10.224.251.131:5740`, `:5744`, `:5748`, `:5750`, `:5755` | 5 |

This verifies original destination forwarding to the intercepted IP and port, socket egress through Wi-Fi and tether, and concurrent relaying. The observed distribution was 3/5 for this run; it is not evidence of an even split or a throughput gain.

## Cleanup

The temporary `1.1.1.1/32` route was removed and its absence verified. Packet Monitor stopped successfully. The prototype process was stopped and its `NetmergePrototype` adapter was removed. The pre-existing `net-merge` adapter remained up; no changes were made to it or to other existing routes.

## Scope and next validation

This is a functional parallel-request check, not a controlled performance benchmark. The small public DoH responses and internet path do not establish sustained throughput, CPU or memory use, or long-lived TCP pinning. Half-close and backpressure currently have local unit coverage, but sustained live traffic and link-failure behavior still need a controlled endpoint test. The supplied milestone attachment was truncated after “6. Start”, so any requirements beyond that point were unavailable.

The implementation verification completed with six Rust unit tests passing, `cargo fmt --check`, Clippy with warnings denied, and a release build.

## Evidence

- `target/m1a-live-results.txt` — individual HTTP results, elapsed time, route and capture cleanup.
- `target/m1a-egress.etl` — raw Packet Monitor capture.
- `target/m1a-egress.txt` — decoded capture used to identify physical egress endpoints.
- `docs/MILESTONE-1A-ARCHITECTURE-AUDIT.md` — implementation and stack-level limits.
