# Milestone 1A.2 implementation audit

## Existing implementation

The production prototype is one Tokio binary. A Wintun-backed `tun` device feeds the vendored `ipstack` TCP/IP stack. `IpStackStream::Tcp` exposes the intercepted source and destination socket addresses. The engine selects an interface from an in-process round-robin pool, creates a Tokio TCP socket, sets the Windows `IP_UNICAST_IF` / `IPV6_UNICAST_IF` option, connects with a timeout, and relays in both directions. Each relay direction shuts down its writer after EOF so the other direction may finish.

`FlowManager` tracks active flows by source, destination, and protocol. Each `FlowRecord` has an ID, interface index, creation time, state, byte counters, duration, and last-activity timestamp. Logging records accepted/closed/failed flows, outbound local and peer endpoints, and average directional throughput. Application identification is unavailable; identifying the originating process requires a later WFP integration.

## Existing six unit tests

- Two flow tests check five-tuple distinction and safe removal by both key and ID.
- Two scheduler tests check rotation/quarantine and ensure a remote connection refusal is not classified as a local interface failure.
- Two relay tests check normal half-close with byte counters and large transfers through tiny in-memory duplex buffers.

These are useful component tests, not end-to-end Windows/TUN tests. In-memory duplex capacity is a proxy for backpressure; it does not measure OS socket queues or process memory. The tests do not exercise cancellation, partial writes as a distinct case, RST behavior, retry recovery, sustained traffic, or route/pktmon automation.

## Working functionality

Milestones 0 and 1A have live capture evidence for destination recovery and forwarding via both Wi-Fi and USB tether. The eight-flow validation returned HTTP 200 for all requests and captured egress on both adapters. `docs/MILESTONE-1A-RESULTS.md` records that run and its 3/5 observed split. The engine limits relay tasks, times out connect, tracks flows, and uses independent half-close-aware relay directions.

## Defects and limitations to address

- Interface health can only change through connect errors and cooldown; tests cannot mark a path unavailable/degraded/recovering at runtime.
- Flow logs do not yet include the outbound socket's local endpoint, which is needed to correlate full tuples with Packet Monitor.
- No deterministic test protocol/server/client or external-data budget exists.
- `ipstack` has unbounded internal channels beyond the application relay semaphore, so bounded relay copy buffers do not establish a whole-process memory bound.
- The current live DoH test is not a controlled benchmark and does not establish sustained pinning, fairness, or aggregation gain.
- The user-space TCP stack remains a protocol risk; no changes to it are proposed without a reproducible failure.

## Planned modifications and test strategy

Add an independent authenticated Tokio test server and a client with explicit byte budgets, transfer caps, JSON results, deterministic payload checksums, and full-duplex/half-close modes. Add a loopback-only development health-control surface to test interface selection without modifying physical adapters. Add outbound local socket endpoints to flow logs. Use local integration tests for protocol integrity, limits, cancellation, and health transitions; use Windows route and Packet Monitor automation only after an external server endpoint and an explicit data budget are configured. Run the requested workspace formatting, lint, test, and release build checks.

No external server is deployed by this project. Real five-minute soak, interface-specific throughput, or public-network performance cannot be claimed until a VPS endpoint is supplied and those tests are run.
