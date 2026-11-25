# Windows relay-socket routing options

## Decision

The current Windows route-based TUN capture cannot safely forward an arbitrary destination through a physical interface while the same destination is routed into the TUN for client capture. The observed Wi-Fi relay SYN was recaptured by the TUN even though the socket's `IP_UNICAST_IF` option was set and the selected Wi-Fi source address was correct. Do not treat a scheduler decision, interface bind, or successful direct Windows connection as proof that Pathweave's relay packet reached the physical adapter.

For a production Windows interception path, investigate WFP ALE connect redirection as the leading supported design. That implies an explicit change from route-based TUN interception for redirected TCP flows: redirect client connects to a Pathweave local proxy/engine, and let its separately created relay sockets use the ordinary physical routing table and `IP_UNICAST_IF`. A WFP callout driver and a redirect-aware user-mode service are required; this is a substantial platform integration, not a filter-only patch. UDP requires a separate redirect design and must not be claimed solved by the TCP path.

No relay-routing fix has been implemented by this investigation. The recommendation is a design gate, not a tested implementation. Before adopting it, prototype the minimum signed/development WFP callout and proxy path, prove original destination recovery and process identity, then correlate a bounded relay tuple to bidirectional NDIS payload on the selected adapter. Preserve the TUN engine behind a transport/interception boundary only if the prototype proves it can be reused without installing a conflicting route.

## Options considered

| Mechanism | What it does | Finding for this failure | Tradeoff |
| --- | --- | --- | --- |
| `IP_UNICAST_IF` / `IPV6_UNICAST_IF` | Selects the outgoing interface for a socket. | Already set successfully; did not prevent the more-specific TUN destination route from recapturing the relay. | Useful for physical egress selection after interception is separated; not a TUN bypass. |
| WFP permit filter | Participates in WFP permit/block arbitration. | Does not install a different route or choose a different egress interface. | Not a route exemption. A broad permit filter also risks interacting with system firewall policy. |
| WFP ALE connect redirection | Redirects a supported TCP/UDP connection request to a proxy endpoint. | The supported interception alternative that can distinguish client connects and avoid the destination `/32` TUN route for the relay, provided route-based capture is removed for those flows. | Requires WFP callout driver lifecycle/signing, redirect-record handling, loop prevention, original-destination recovery, service integration, and separate UDP coverage. |
| Windows network compartment | Gives interfaces and routes a compartment-specific routing context. | Not selected: `SetCurrentThreadCompartmentId` and `GetCurrentThreadCompartmentId` are documented as reserved for future use. No supported thread-local socket escape was established. | A compartment would also need usable physical interfaces and routes; moving host adapters could disrupt unrelated connectivity. |
| Destination host-route exception | Sends a destination outside the TUN. | Breaks capture for ordinary client traffic to that same destination. | Cannot distinguish the client socket from Pathweave's relay socket. Not acceptable for arbitrary destinations. |
| Source-address bind / socket bind-to-device | Constrains source/interface selection. | Existing `IP_UNICAST_IF` and observed Wi-Fi local address already demonstrate this does not solve the route collision. | Keep as an egress constraint after routing/interception is separated. |
| Increase connect timeout or suppress repeated flow count | Masks symptoms. | Does not make the missing physical SYN appear. | Rejected. |

## Evidence and sources

- Live root-cause evidence: [WIFI-OUTBOUND-DIAGNOSIS.md](WIFI-OUTBOUND-DIAGNOSIS.md). It records the original flow, relay tuple re-entering TUN, direct socket control, capture result, timeout interpretation, and cleanup.
- Microsoft documents `IP_UNICAST_IF` as the outgoing interface socket option: [IPPROTO_IP socket options](https://learn.microsoft.com/en-us/windows/win32/winsock/ipproto-ip-socket-options).
- Microsoft documents WFP permit/block classification and callout arbitration: [WFP basic operation](https://learn.microsoft.com/en-us/windows/win32/fwp/basic-operation) and [About WFP](https://learn.microsoft.com/en-us/windows/win32/fwp/about-windows-filtering-platform).
- Microsoft documents connect/bind redirection as a WFP ALE callout-driver feature, including TCP and UDP support and redirect records: [Using bind or connect redirection](https://learn.microsoft.com/en-us/windows-hardware/drivers/network/using-bind-or-connect-redirection).
- Microsoft explicitly marks `SetCurrentThreadCompartmentId` as reserved for future use: [SetCurrentThreadCompartmentId](https://learn.microsoft.com/en-us/windows/win32/api/netioapi/nf-netioapi-setcurrentthreadcompartmentid). The analogous getter is also reserved: [GetCurrentThreadCompartmentId](https://learn.microsoft.com/en-us/windows/win32/api/netioapi/nf-netioapi-getcurrentthreadcompartmentid).

## Current live adapter snapshot

At investigation time, Wi-Fi is Up at ifIndex 8 (GUID `{23F040A9-6336-45F6-99C5-D8229FE61C22}`), IPv4 `192.168.1.100/24`, gateway `192.168.1.1`. The Remote NDIS adapter named Ethernet 2 is also Up at ifIndex 4 (GUID `{0D93791B-0078-40CB-AF59-2918EED88F56}`), IPv4 `10.224.251.197/24`, gateway `10.224.251.118`. The route query showed active default routes through both interfaces; this snapshot is not used to infer which route any particular socket selects. The machine reports only the default compartment (ID 1). These indices and addresses are point-in-time observations and must be resolved dynamically for every test. No route, adapter, QoS, or data-plane setting was changed in this investigation.

