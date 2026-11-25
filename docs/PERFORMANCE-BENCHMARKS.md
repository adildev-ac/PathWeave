# TCP performance benchmarks

## Current results

No Wi-Fi-only, USB-only, or combined WAN throughput benchmark has been run for M1A.2. The local loopback protocol exercise validates framing and integrity; its speed is not representative of either physical link. The previous public DoH run consisted of eight small HTTPS requests over 2,268 ms and is only a functional check.

Do not describe combined throughput as aggregation until repeated controlled VPS measurements exceed the faster single-link result under the same endpoint, workload, and time window.

## Reproducible benchmark matrix

Use the same private server, explicit token, and byte budget for each run. On Windows, `scripts/windows/test-tcp-soak.ps1` accepts an exact interface selection only through adapter discovery (Wi-Fi plus Remote NDIS); use the engine's `--interface` argument directly for Wi-Fi-only or USB-only runs after rediscovering the indexes. Capture each run and preserve its JSON, ETL, decoded packet text, and process-profile CSV.

Record at least three trials after one warm-up for each interface mode, direction, and connection count (1, 2, 4, 8, 16). Alternate single-link and combined trials where practical. For each, record upload/download application bytes, elapsed time, throughput, latency, client/server checksums, CPU time, peak/private memory, and per-interface source address / five-tuple evidence. Use application and server byte counters as the transfer measurement; use Packet Monitor to confirm which physical path carried each flow.

| Configuration | Download | Upload | Duplex | CPU/memory | Per-link capture |
| --- | --- | --- | --- | --- | --- |
| Wi-Fi only | Not run | Not run | Not run | Not run | Not run |
| USB tether only | Not run | Not run | Not run | Not run | Not run |
| Wi-Fi + USB | Not run | Not run | Not run | Not run | Not run |

No multi-gigabyte test runs automatically. Choose `--max-bytes` and `--rate-bytes-per-sec` explicitly, especially for USB tethering.
