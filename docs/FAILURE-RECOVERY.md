# Interface health and failure recovery

## Scheduler behavior

New flows use round-robin selection among `Healthy` interfaces. `Degraded` interfaces are eligible only when no healthy interface is available. `Unavailable` and `Recovering` interfaces are excluded. A definite local path failure still applies a timed unavailable cooldown; a normal remote refusal does not quarantine the path. `mark_healthy` and the development health API clear the cooldown.

The development control listener is disabled by default and rejects non-loopback addresses. Enable it with `--health-control-listen 127.0.0.1:47832`. Commands use one line: `GET`, or `SET <ifIndex> healthy|degraded|unavailable|recovering`. The included PowerShell helper sends these commands. Do not bind a control API on a LAN interface.

Health changes affect new flow selection. They do not migrate established TCP sockets. A failed connection generally cannot move transparently between Wi-Fi and USB tethering because the new path can have a different source address and TCP four-tuple; the application must report that flow as failed and the client may reconnect at a higher layer.

## Validation status

Unit tests exercise unavailable/recovering exclusion, degraded fallback, healthy recovery, round-robin, and refusal classification. The local protocol integration run exercises normal disconnect and half-close, not a live physical-path outage or RST injection. No physical adapter was disabled or disconnected. Live midstream read/write failure, Windows route changes during a soak, and recovery timing remain unvalidated.
