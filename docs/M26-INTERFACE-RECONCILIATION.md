# M2.6 Windows interface reconciliation

Windows `GetIfTable2` snapshots refresh GUID, LUID, current index, alias, operational state, media/link counters, and link speed. `GetUnicastIpAddressTable` and `GetIpForwardTable2` are polled with the counter collector to refresh current IPv4/IPv6 addresses and default-route next hops. The polling worker runs outside native callbacks; a fresh snapshot is reconciled about every two seconds. No routes, DNS settings, or physical adapter state are changed.

Pathweave matches configured adapters by GUID. A changed index updates new-flow selection, probes, and QoS rate application. A missing configured adapter is marked unavailable; existing sockets remain pinned and may fail. A returning adapter with the same GUID becomes degraded and is subject to normal health checks before new assignments. Address and gateway snapshots are diagnostic and refresh as telemetry; socket egress is selected through Windows interface binding.

The current implementation uses periodic snapshots rather than `NotifyIpInterfaceChange`, `NotifyUnicastIpAddressChange`, or `NotifyRouteChange2`. Startup still requires each selected interface to be present; preserving a configured but absent GUID as an offline startup slot is not yet supported. The Android tether adapter is not currently present on the validation host.
