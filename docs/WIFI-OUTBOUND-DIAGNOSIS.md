# Wi-Fi outbound TCP diagnosis

## Finding

The Wi-Fi adapter and ordinary Windows TCP connectivity are healthy. Pathweave's interface bind also succeeds. The bounded Pathweave flow fails because its relay connection to the test destination is sent back into Pathweave's TUN by the temporary destination `/32` route. That creates recursive interception instead of a physical Wi-Fi handshake.

The diagnostic observed the loop directly: correlation 1 accepted the original flow from `198.18.0.1:7681` to `172.66.147.243:443`, selected Wi-Fi, and began an interface-bound connect. The engine's pending TCP socket appeared on Wi-Fi as `192.168.1.100:7682 -> 172.66.147.243:443` in `SynSent`. Correlation 2 then arrived back from the TUN with that exact tuple and was rejected at the one-flow diagnostic limit. The same rejected tuple recurred under correlations 3 and 4. No relay was initialized.

The temporary `172.66.147.243/32` route pointed to Pathweave ifIndex 48 while the normal default route remained Wi-Fi ifIndex 8. The socket's `IP_UNICAST_IF` option was accepted for ifIndex 8, and Windows selected a Wi-Fi source address, but this does not exempt the relay socket from the more specific TUN route in this test arrangement. Microsoft documents `IP_UNICAST_IF` as selecting an outgoing IPv4 interface and requires the index in network byte order; Pathweave already encodes it that way and the regression test now covers the encoding. [Microsoft IPPROTO_IP socket options](https://learn.microsoft.com/en-us/windows/win32/winsock/ipproto-ip-socket-options)

## Live reproduction

USB tethering was disconnected. The diagnostic dynamically resolved Wi-Fi from its current adapter GUID and Pathweave telemetry:

| Property | Observed value |
| --- | --- |
| Alias | Wi-Fi |
| GUID | `23f040a9-6336-45f6-99c5-d8229fe61c22` |
| LUID | `19985273102270464` |
| Interface index | `8` |
| IPv4 address | `192.168.1.100/24` |
| Gateway | `192.168.1.1` |
| Status | Up |

An ordinary Windows TCP socket bound to Wi-Fi connected to `172.66.147.243:443` in 54 ms: `192.168.1.100:7679 -> 172.66.147.243:443`. Pathweave's separate outbound socket was `192.168.1.100:7682 -> 172.66.147.243:443`, owned by the engine and observed in `SynSent`; it did not complete.

The bounded HTTPS request returned curl error 28 after 5.008 seconds with zero response bytes. Pathweave's connect future remained pending until its configured 12-second deadline. It ended with a Tokio `TimedOut` error and no native Winsock error code because Windows had not completed or rejected the connect when the deadline canceled it.

The netsh NDIS trace converted to 17,010 XML provider lines. Neither the destination IPv4 bytes (`AC4293F3`) nor its text address appeared in that capture, so it provides no matching physical SYN, SYN-ACK, ACK, or TLS payload evidence. Physical egress is not verified.

## What the previous “512 timeouts” counted

`--max-connections` defaults to 512 and creates a Tokio semaphore with that many permits. In the earlier unrestricted run, the engine accepted 512 flow records, each produced one interface-connect timeout, and three more incoming flows were rejected when the semaphore was full. Those are not a separate retry or probe counter: they are per-flow connect timeout events while as many as 512 flows were pending. The sequential source ports and this one-slot reproduction show that the pending relay sockets are recursively re-entering the TUN.

The bounded reproduction sets `--max-connections 1`. Its three repeated capacity rejections use the same relay tuple, which prevents the recursion from consuming hundreds of relay slots while preserving evidence of the loop.

## Fix boundary

No data-plane or QoS change was made. The current route-based TUN setup has no per-socket or per-process exemption for Pathweave's own relay sockets. A route for the destination that sends client traffic through the TUN also sends Pathweave's same-destination relay traffic back through that TUN, despite `IP_UNICAST_IF` being set. Binding the relay socket to the Wi-Fi source address does not resolve this route collision.

There is no safe one-line fix within the current route-only interception mechanism. Forwarding needs a way to distinguish original client traffic from Pathweave-owned sockets, such as a Windows Filtering Platform exemption or another explicitly isolated routing path. That is a separate routing integration; it was not introduced as part of this diagnosis. Until then, the Wi-Fi-only test must remain failed, and the bounded diagnostic's one-slot cap remains a test guard rather than a forwarding fix.

## Evidence and regression checks

The generated report and capture are under `target/live-validation/`:

- `pathweave-if8-20261125-232601-result.json` — identity, direct and Pathweave socket tuples, route snapshots, failure, and cleanup.
- `pathweave-if8-20261125-232601-engine.log` — newline-delimited JSON lifecycle events, including correlation IDs and the recaptured tuple.
- `pathweave-if8-20261125-232601.etl` and `pathweave-if8-20261125-232601-packets.xml` — netsh NDIS capture and conversion.

The repeatable bounded diagnostic is `scripts/windows/diagnose-pathweave-wifi.ps1`. It resolves the current Wi-Fi GUID/index/LUID, refuses to run while a Remote NDIS tether is up, records a direct bound TCP connection, starts Pathweave with one relay slot, captures the Wi-Fi NDIS path, samples engine-owned TCP sockets, and verifies cleanup.

`cargo test` passed 51 engine tests and four protocol-tool tests. New regression assertions cover GUID/LUID/index identity refresh and Windows interface-index byte order. PowerShell parser checks passed for the bounded test scripts. The live reproduction intentionally remains a failure until relay-socket routing can bypass the TUN without changing the system default route.
