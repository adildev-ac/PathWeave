# Stable Interface Identity

At startup, configured `--interface` indexes are resolved against a Windows `GetIfTable2` snapshot. Each configured path is then tracked by normalized interface GUID, with the current `NET_LUID_LH.Value`, index, alias, and operational status retained for diagnostics. The index is resolved from the latest row whenever new flows are assigned or a socket is created. The GUID is the configuration key; the alias is display metadata only.

`--qos-interface-guid-limit GUID=UPLOAD:DOWNLOAD` attaches a QoS limit to a GUID. The compatible `--qos-interface-limit INDEX=UPLOAD:DOWNLOAD` is resolved to a GUID at startup and logs that it is temporary. No GUID is inferred from a duplicate alias. A GUID absent from discovery is not substituted with another adapter.

The interface reconciler compares configured GUIDs against the telemetry table every two seconds. It refreshes LUID/index, marks absent or down paths unavailable for new flows, reapplies rates when a configured GUID returns, and makes a returning path degraded until a successful connection marks it healthy. Adaptive probe workers also resolve the current index from their stable GUID before each probe. Established TCP/UDP sockets keep their existing assignments and are never migrated.

The current snapshot comes from `GetIfTable2`, whose row already contains GUID, LUID, and current index. There is no separate ConvertInterfaceLuidToIndex call because the row is the current resolution. Address and gateway-change tracking is not yet implemented; socket-binding errors and adapter operational status are used for readiness. This is a runtime identity registry, not a persisted TOML configuration store.
