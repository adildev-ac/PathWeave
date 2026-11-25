# M2 architecture audit

## Existing implementation

- `InterfacePool` stores the explicitly configured Windows interface indexes, round-robin cursor, health state, and path-error cooldown.
- `next_available()` is synchronous and uses small per-interface standard mutexes. It prefers Healthy, then Degraded, and skips Unavailable and Recovering paths.
- TCP and UDP dispatchers select once, create a `FlowRecord` with that index, and bind the outbound socket to it. Existing flows are not moved.
- `FlowManager` records application-level directional bytes and flow lifecycle. The pool's health control endpoint is loopback-only.
- There was no adapter discovery, operating-system counter collection, latency probe, traffic class, or scheduler configuration.
- The M2 one-shot counter view reports OS adapter names/GUIDs but does not replace explicit adapter selection for forwarding.

## M2 changes

- A shared `FlowScheduler` receives protocol, tuple, and traffic class metadata through the existing transport-engine dispatch boundary.
- `InterfacePool` remains the source of configured indexes and health eligibility. Windows `GetIfTable2` telemetry is collected independently and published as cloned immutable snapshots.
- Counter sampling uses monotonic elapsed time and calculates adapter-level receive/send rates. These counters include all host traffic on the adapter, not only Netmerge flows. Flow byte counters remain separate.
- Optional per-interface TCP connect-duration probes use the existing interface-bound connect path. They report connection-establishment duration, not pure RTT, and failures do not quarantine an interface.
- Modes are `round-robin` (default), `weighted`, `adaptive`, and `adaptive-shadow`. Shadow routing always uses round-robin while logging a separate adaptive recommendation.
- Selection is flow-level. TCP/UDP handlers continue to pin the socket to the selected interface for the lifetime of that flow.

## Constraints found

Interface indexes are still supplied by the operator and are used at socket bind time. Telemetry captures a stable Windows interface GUID next to each sampled index, so index changes can be noticed, but automatic GUID-to-current-index re-resolution and adapter discovery are not implemented yet. The probe target is a single optional TCP endpoint. No WFP, payload inspection, GUI, route mutation, or multipath gateway is part of M2.
