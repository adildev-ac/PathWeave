# TCP soak and protocol testing

## Local protocol verification

Build and run the self-contained local tests without administrator privileges or external data use:

```powershell
cargo build --bins
.\scripts\test-local-protocol.ps1
```

The script starts the test server only on `127.0.0.1`, creates a temporary random token, then verifies download, upload, duplex, half-close, four and fifty concurrent duplex sessions, a two-second rate-limited duplex session, 1,000 sequential uploads, client/server byte-limit rejection, and authentication rejection. It writes machine-readable samples under a unique `target/local-protocol-results-*` directory and stops its server process in `finally`.

## Live TUN test

After deploying the controlled server on a private VPS/VPN endpoint and setting `NETMERGE_TEST_TOKEN`, build release binaries. Run an elevated PowerShell session and choose a byte cap that is acceptable for the metered link:

```powershell
cargo build --release --bins
.\scripts\windows\test-tcp-soak.ps1 -ServerAddress 10.20.0.1:47800 -AllowExternal -Mode duplex -Bytes 1048576 -MaxBytes 2097152
```

The script rediscovers active Wi-Fi and Remote NDIS adapters, rejects a conflicting host route, pre-existing `NetmergePrototype`, or existing Packet Monitor session, creates only a destination `/32` route, captures NIC packets, profiles the engine process, and cleans up only its own route, capture, process, and TUN. It snapshots default routes and the pre-existing `net-merge`/`utun8` adapter states and verifies they remain unchanged. Results are written under `target/m1a2`.

Long connection tests use `-Mode duplex -DurationSeconds 300 -MaxBytes <explicit-total-budget>`. `-Connections` and `-Concurrency` control session count and overlap. `--rate-bytes-per-sec` caps sending, while `--receive-rate-bytes-per-sec` makes the receiver consume slowly for backpressure tests. The profiler CSV records CPU time, working/private/peak memory, handles, threads, and TCP socket count at one-second intervals. Packet captures are path evidence; use JSON client bytes and remote server counters for throughput, not packet-count sums.

Runtime health overrides are development-only and loopback-only. Start the engine with `--health-control-listen 127.0.0.1:47832`, then use:

```powershell
.\scripts\windows\set-interface-health.ps1 -Command GET
.\scripts\windows\set-interface-health.ps1 -Command SET -InterfaceIndex 4 -Status unavailable
.\scripts\windows\set-interface-health.ps1 -Command SET -InterfaceIndex 4 -Status recovering
.\scripts\windows\set-interface-health.ps1 -Command SET -InterfaceIndex 4 -Status healthy
```

Unavailable and recovering interfaces receive no new flows. Degraded interfaces are selected only when no healthy interface is eligible. Existing flows retain their already chosen interface. No script disables an adapter or modifies a default route.

Real five-minute soak and external link-failure tests remain pending until a remote test server is configured. Do not disconnect a physical interface as part of an automated run.
