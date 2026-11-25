# Pathweave remote TCP test server

`pathweave-test-server` is a separate Tokio binary. It is not started or linked by the TUN engine. It accepts one bounded test session per TCP connection and logs newline-delimited JSON connection records.

## Security and deployment

The server defaults to `127.0.0.1:47800`. A token of at least 24 characters is required through `PATHWEAVE_TEST_TOKEN`; do not pass the secret on the command line. A non-loopback bind also requires one or more exact `--allow-source-ip` restrictions. Configure a firewall to admit only the test client. The protocol is plain TCP, so run remote tests over a private WireGuard/VPN path; do not expose the port to the public Internet or use the token across an untrusted network. The server is not deployed automatically.

On a Linux VPS, install Rust, copy the repository, then:

```sh
export PATHWEAVE_TEST_TOKEN="$(openssl rand -hex 32)"
cargo build --release --bin pathweave-test-server
./target/release/pathweave-test-server \
  --listen 10.20.0.1:47800 \
  --allow-source-ip 10.20.0.2 \
  --max-connections 16 \
  --max-bytes 67108864 \
  --max-session-secs 600
```

Use the server's private VPN address for `--listen` and allow only the client's VPN address. Keep the token in the service manager's protected environment, not in a checked-in file. Stop with Ctrl+C; the server stops accepting and gives active sessions up to five seconds to finish.

The server enforces the concurrent connection cap, requested per-direction byte cap, maximum session lifetime, client token, and optional source-IP allow list. Its default transfer cap is 64 MiB per direction per connection and its default session limit is ten minutes. Each connection logs ID, peer IP and port, local destination port, operation, bytes in each direction, elapsed time, and termination reason. Packet payloads use a deterministic byte pattern; client/server SHA-256 results are cross-checked.

## Client examples

Set the same token in the client environment. A Windows client can run:

```powershell
$env:PATHWEAVE_TEST_TOKEN = '...'
.\target\release\pathweave-test.exe --server 10.20.0.1:47800 --mode duplex --bytes 1048576 --max-bytes 2097152 --allow-external --output target\duplex.json
```

`--max-bytes` is mandatory and counts application payload bytes across all flows and directions. `--bytes` is a per-connection fixed amount; `--duration-secs` is an alternative and remains capped by the explicit budget. `--connections` creates that many sessions; `--concurrency` controls how many run at once. `--rate-bytes-per-sec` throttles sending; `--receive-rate-bytes-per-sec` slows the receiving side to exercise backpressure. Ctrl+C cancels the run and closes active sockets. Non-loopback destinations require `--allow-external` in addition to the explicit cap.

Modes are `download`, `upload`, `duplex`, and `half-close`. Half-close sends a deterministic payload, shuts down the client write half, waits for the server to observe EOF, then verifies the complete response. The client prints JSON and can write the same report to `--output`.

The protocol intentionally does not implement TLS. Keep it on a private VPN and firewall boundary. Do not use it as an open Internet speed-test service.
