# M2.6 runtime QoS control

The optional TCP control listener is restricted to loopback. `GET` returns the current rate-policy snapshot. Mutating `SET-GUID`, `SET-INDEX`, and `ROLLBACK` requests require `AUTH <token>`; set `PATHWEAVE_QOS_CONTROL_TOKEN` to a randomly generated secret of at least 32 bytes before enabling `--qos-control-listen`. The token is never logged. Requests are limited to 1 KiB and time out after three seconds.

Example request lines:

```text
GET
AUTH <token> SET-GUID <guid> upload 15
AUTH <token> SET-INDEX 8 download 90
AUTH <token> ROLLBACK
```

Index changes resolve to the configured GUID at request time. The policy manager validates and applies directional rates atomically, versions successful changes, and supports one-step rollback. Active flows retain their interface pin.

**Full TOML policy reload is not implemented yet.** Mode, aggregate rate, class weights, queue limits, adaptive controller parameters, AQM settings, and probe settings are startup configuration. Adaptive mode rejects manual effective-rate overrides because its controller owns those rates. Do not treat this endpoint as a complete configuration reload API.
