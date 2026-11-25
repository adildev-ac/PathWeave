# UDP configuration

UDP relay is disabled unless `--enable-udp` is supplied.

| Option | Default | Meaning |
| --- | ---: | --- |
| `--enable-udp` | off | Enable UDP forwarding. |
| `--max-udp-sessions` | 2048 | Maximum active UDP flows; excess new flows are dropped. |
| `--udp-queue-capacity` | 8 | Maximum queued inbound datagrams per flow. |
| `--udp-max-datagram-bytes` | 1472 | Maximum application payload accepted in either direction. Must be positive and at most `MTU - 28` for IPv4. |
| `--udp-idle-timeout-secs` | 120 | Session lifetime after the last forwarded datagram. |

Example:

```powershell
cargo run -- --interface 8,14 --enable-udp --max-udp-sessions 2048 --udp-queue-capacity 8 --udp-max-datagram-bytes 1472 --udp-idle-timeout-secs 120
```

The payload limit prevents the engine from accepting data that cannot fit within the configured IPv4 TUN MTU once IP and UDP headers are added. The vendored stack independently checks response size and returns an error instead of truncating a datagram.
