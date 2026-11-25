# Netmerge engine prototype

This repository currently contains a Windows-first feasibility prototype for TCP and IPv4 UDP. It uses the real engine path: TUN packets → userspace IP stack → original destination recovery → interface selection → interface-bound transport relay.

The current program is not a complete system traffic router. It does not install routes, restore route state, or discover adapters. UDP forwarding is disabled by default and can be enabled with `--enable-udp`. Do not route all system traffic through it.

New flows use `round-robin` by default. M2 adds opt-in `weighted`, `adaptive`, and `adaptive-shadow` modes, with shadow mode retaining round-robin for actual routing. See [docs/ADAPTIVE-SCHEDULING.md](docs/ADAPTIVE-SCHEDULING.md) and [docs/NETWORK-TELEMETRY.md](docs/NETWORK-TELEMETRY.md).

## Build

Install the stable Rust toolchain and the Windows MSVC C++ build tools. Then run:

```powershell
cargo check
cargo test
cargo run -- --help
```

The first dependency candidate is `ipstack` 1.0.1 (Apache-2.0). Live Wi-Fi + Android USB tether results are in [docs/MILESTONE-0-RESULTS.md](docs/MILESTONE-0-RESULTS.md). Its documentation describes it as unstable and under development, so the live protocol acceptance checks in [docs/MILESTONE-0.md](docs/MILESTONE-0.md) are required before making a production stack decision.

## Hardware test

Windows TUN creation requires administrator rights and the Wintun driver. The app takes physical interface indexes explicitly; identify them with:

```powershell
Get-NetAdapter | Format-Table Name, InterfaceIndex, Status, InterfaceDescription
```

The Windows build uses the signed Wintun DLL supplied by the `wintun` Rust dependency and verifies its availability beside the executable under `examples\wintun.dll`.

Start the TCP prototype with participating indexes, for example:

```powershell
cargo run -- --interface 8,14
```

Use a controlled remote TCP test host. After the TUN interface appears, add only that host's `/32` route through the TUN, run multiple independent client connections to it, and remove the route immediately after testing. Capture traffic on the Wi-Fi and USB tether adapters to verify egress. Do not use a default route for this prototype.

The application uses Windows `IP_UNICAST_IF` / `IPV6_UNICAST_IF` on each outbound socket and logs the intercepted source and destination tuple plus selected interface index. It also records the outbound socket endpoints. The logs alone do not prove physical egress; verify it with a packet capture.

To enable the M1B UDP data plane, add `--enable-udp`. It keeps each session pinned to one selected adapter, preserves UDP datagram boundaries, and expires idle sessions. M1B is IPv4-focused; IPv6 UDP is rejected for now. Configuration and limits are in [docs/UDP-CONFIGURATION.md](docs/UDP-CONFIGURATION.md), [docs/UDP-DATA-PLANE.md](docs/UDP-DATA-PLANE.md), and [docs/UDP-SESSION-MANAGEMENT.md](docs/UDP-SESSION-MANAGEMENT.md).

Scheduler examples (weights/capacities use the explicit interface indexes):

```powershell
cargo run -- --interface 8,14 --scheduler-mode adaptive-shadow --interface-weight 8=100 --interface-weight 14=100 --interface-capacity-mbps 8=100:20 --interface-capacity-mbps 14=50:15 --diagnostics-json
```

Switch from observation to adaptive assignment only after reviewing the per-flow shadow decisions and telemetry freshness. See [docs/TRAFFIC-CLASSIFICATION.md](docs/TRAFFIC-CLASSIFICATION.md) for policy rules.

## Milestone 1A.2 test tools

The companion `netmerge-test-server` and `netmerge-test` binaries provide authenticated deterministic upload, download, duplex, and half-close tests with explicit transfer budgets and SHA-256 checks. Build and exercise the local protocol without admin rights or WAN data:

```powershell
cargo build --bins
.\scripts\test-local-protocol.ps1
```

For the controlled remote server, protocol security, soak instructions, health overrides, and current limits, see [docs/REMOTE-TEST-SERVER.md](docs/REMOTE-TEST-SERVER.md), [docs/TCP-SOAK-TESTING.md](docs/TCP-SOAK-TESTING.md), and [docs/MILESTONE-1A2-RESULTS.md](docs/MILESTONE-1A2-RESULTS.md). External testing requires a configured private test server, `--allow-external`, and an explicit `--max-bytes` budget.

## Scope

- Milestone 0/1 spike: TCP transparent interception and interface binding.
- Milestone 1B: opt-in IPv4 UDP forwarding on a per-flow pinned interface.
- Round-robin assignment applies to new flows; each relay stays on its assigned interface.
- The transport-engine boundary keeps TCP, UDP, and future transport implementations separate from interface selection and health.
- Adaptive scheduling has not been implemented; Product B multipath bonding requires a remote gateway and remains out of scope.
- No GUI work has started.

