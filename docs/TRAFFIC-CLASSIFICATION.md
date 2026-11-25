# Traffic classification

Classification uses the recovered transport protocol, destination port, and optional destination CIDR; it does not inspect payloads or claim application identity. Port 53 defaults to DNS. Other traffic, including UDP 443/QUIC, defaults to Balanced because QUIC can carry bulk downloads.

CLI rules are evaluated in the order supplied and the first match wins:

```powershell
--traffic-rule tcp:203.0.113.0/24=bulk --traffic-rule udp:443=interactive --traffic-rule any:5353=dns
```

Supported classes are `interactive`, `bulk`, `balanced`, and `dns`. A protocol may be `tcp`, `udp`, or `any`; a destination selector may be a port, IPv4/IPv6 CIDR, or `*`. Persisted TOML policy files are not implemented in this iteration. Process/application routing remains a later WFP milestone.
