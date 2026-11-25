# Milestone 1A architecture audit

**Scope source:** The supplied milestone attachment ends mid-sentence at “6. Start”. This audit and implementation follow the complete requirements visible before that truncation; any omitted sections (for example, later acceptance commands or perf targets) were unavailable.

## Existing architecture

This is a single Rust binary, not a Cargo workspace. `src/main.rs` creates the TUN through `tun` 0.8, configures `ipstack` 1.0.1 as the userspace TCP/IP stack, accepts `IpStackStream::Tcp`, obtains the intercepted source and destination from `local_addr()` / `peer_addr()`, picks an interface by round-robin, creates a Tokio `TcpSocket`, applies Windows `IP_UNICAST_IF` or `IPV6_UNICAST_IF`, connects, then relays with Tokio's `copy_bidirectional`.

Wintun is loaded from the signed DLL shipped by the `wintun` dependency and copied by the vendored `ipstack` build script. That local vendor patch only creates the expected Cargo examples output directory before copying the DLL.

## Verified behavior

The Milestone 0 results and capture show four separate TCP/TLS flows to `1.1.1.1:443`, with two relayed through Wi-Fi and two through Android USB tether. The TUN ingress used `198.19.0.1`, while the captured outbound source addresses were the selected adapters' assigned IPv4 addresses. The test's `/32` route was removed and the test process/TUN were stopped. See [MILESTONE-0-RESULTS.md](MILESTONE-0-RESULTS.md) and the `target/m0-egress.*` artifacts.

## Gaps addressed in Milestone 1A

- The old loop spawned detached tasks without a concurrency limit or shutdown tracking.
- There was no flow key/table, ID, state, byte counter, lifetime, or last-activity metric.
- Round-robin considered every configured index usable forever; no reactive interface health state existed.
- Relay behavior had not been explicitly tested for one-sided EOF / TCP half-close.
- Connect attempts had no timeout; relay byte counts and errors were only visible after completion.
- No performance benchmark or controlled long-lived test had been run.

## Stack-level limits and risk

The vendored `ipstack` is a custom, unstable TCP/IP implementation. Its `accept_sender`, packet sender, per-flow TCP packet sender, and TCP data channel are Tokio unbounded channels. Its TCP control block has fixed read and unacknowledged-byte limits, but those do not bound every unbounded queue. A stalled stack consumer or peer may therefore still grow memory outside this application's relay semaphore. `IpStackTcpStream::drop` also synchronously waits for its internal task with `block_in_place`, which can add shutdown latency and must be considered when evaluating stalls. The user-space TCP implementation itself remains a protocol-correctness risk and has not been replaced in this milestone.

## Dependencies

| Dependency | Current purpose |
| --- | --- |
| `ipstack` (vendored 1.0.1, Apache-2.0) | Async TUN userspace TCP/UDP stack; provisional and unstable. |
| `tun` | TUN creation and async device I/O; Windows Wintun backend. |
| `tokio` | Runtime, TCP sockets, relay I/O, semaphore, task tracking, timers, Ctrl+C. |
| `windows-sys` | Windows Winsock interface binding socket options. |
| `clap` | Prototype command-line settings. |
| `tracing`, `tracing-subscriber` | Flow and engine diagnostics. |
| `anyhow` | Top-level startup and configuration errors. |

There is no serde config, logging IPC, adapter discovery, persistent flow history, GUI, or production route manager yet.

## Milestone 1A changes

The engine now tracks active TCP flows by a five-tuple and assigns a monotonically increasing flow ID. Relay tasks are bounded by `--max-connections`, outbound connects time out, and definite local path failures quarantine an interface briefly. The relay uses two independently half-closing copy directions so one side's FIN does not discard the other side's response. Successful writes update directional byte counters and activity time. Ctrl+C stops acceptance, drains flows briefly, then aborts remaining tasks.

These changes bound the application relay layer. They do not fix `ipstack`'s unbounded internal channels, add OS adapter discovery, automatically restore routes, or implement UDP/production IPv6.

## Recommended next work

1. Run sustained tests against a controlled endpoint and measure throughput, per-link bytes, CPU, and memory; include a long-lived flow to confirm pinning over time. The short live parallel check is documented in [MILESTONE-1A-RESULTS.md](MILESTONE-1A-RESULTS.md).
2. Test link removal/recovery while flows exist; new flows should avoid quarantined links while active flows retain their assignment.
3. Audit and stress `ipstack` queue behavior. If queue memory cannot be bounded or TCP conformance fails, compare/replace the userspace stack before adding more product layers.
4. Build a Windows route journal and cleanup/recovery layer before broadening capture beyond destination-specific test routes.
