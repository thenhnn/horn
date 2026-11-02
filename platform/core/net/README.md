# Capability-isolated HTTP demo

Run the existing console/shell demo:

```sh
nix-shell --run 'just shell'       # KVM; explicit fallback: just shell tcg
curl --noproxy '*' http://127.0.0.1:8080/
# Hello from Horn!
```

QEMU user networking forwards **127.0.0.1:8080 → guest TCP 80**, without TAP or
root. `restrict=on` limits guest networking; the forward binds localhost only.
The launcher defaults to two virtual CPUs, with automatic process placement;
`just shell kvm 1` retains a single-CPU boot. Port 8080 must be free. Exit the console with **Ctrl-A, then x**.
Kernel/service logs are in `work/horn-debug.log`; `ps` and `registry` show the
new processes/endpoints.

## Processes and authority

1. **virtio-net** receives one PCI device capability and its own VM account.
   Upstream `virtio-drivers = 0.13.0` provides `VirtIONetRaw`; Horn supplies the
   modern ACCESS_PLATFORM/MSI-X transport and directional DMA HAL. The driver
   sees IOVAs, never RAM physical addresses. It implements Ethernet, not IP.
2. **netd** receives Ethernet and clock capabilities. Pinned `smoltcp = 0.12.0`
   owns Ethernet/ARP, DHCPv4, IPv4 and TCP. It receives neither assignment
   authority nor hardware/MMIO/DMA capabilities. READY follows DHCP configuration.
3. **http-demo** receives only TCP service plus ordinary task/VM/ready ports and
   a notification. Its single-threaded event loop reserves port 80 and drives
   multiple independent connections with bounded work per turn. An unfinished
   header or backpressured response does not block other requests. It sends a
   fixed response with Content-Length/Connection: close. There is no network-
   configuration or device authority in the application.

Nix definitions in `platform/system/module.nix` wire dependencies and separate
RAM accounts. `platform/system/console.nix` enables networking in `just shell`;
clock/timerd remain enabled without it. The PCI catalog matches modern
`1af4:1041`, not a fixed BDF. The example uses:
`/net/ethernet/virtio/0000:00:05.0`, `/services/clock`, `/services/net/tcp`.
As elsewhere, paths select published capabilities; paths do not confer authority.

## Bounded APIs and lifetime

Ethernet frames are 14–1514 bytes, transferred in at most 512-byte chunks.
Receive preserves a frame boundary; begin/append/commit stages one transmit
frame. The driver buffers up to 64 RX frames and eight TX frames, so a short
handshake burst is not dropped simply because netd is between RPC turns.
Generation watches are one-shot hints, with recheck/rearm rather than polling.
netd has bounded local queues and bounded ingress work per turn. Reaching that
work budget schedules another drain: it is not mistaken for WouldBlock/empty.
A local-work hint stops once queues are drained or TX backpressure prevents
progress; it is not an idle polling tick.

A TCP factory reserves a port and returns a badged listener capability.
`accept()` returns a **separate connection capability**; no caller-supplied
socket ID selects another stream. The listener only accepts/watches ACCEPT;
connections read/write/watch READABLE/WRITABLE/CLOSED. Read/write parcels are
256 bytes with explicit WouldBlock/Closed. EOF is readable; a peer FIN does not
continuously wake a backpressured writer. Idle TCP timeout is 30 s.

`finish()` queues graceful close and retires the connection capability (including
copies). The client deletes its local cap after success and must not use it
again. netd retains buffers through FIN/TIME_WAIT independently of that handle,
then removes closed sockets and returns their buffer allowance. Storage slots
may be reused; endpoint badges are never reused. There are four lifetime port
reservations; automatic abandoned-handle/client-death cleanup is not implemented.

## Growing TCP pool and budgets

A port starts with one listening socket, not eight preallocated streams. netd
replenishes listeners between ingress packets, including SYNs in the same batch.
Buffers grow on demand for handshakes, accepted streams and closing sockets.
Pending sockets and live capabilities have separate admission limits:

| Nix option (`devmgr-demo.` prefix) | Default | Meaning |
| --- | --- | --- |
| `tcp-buffer-bytes` | 4 MiB | Aggregate requested RX/TX buffer capacity, including FIN/TIME_WAIT |
| `tcp-backlog` | 32 | Pending handshake/accept slots per port, including the standing listener |
| `tcp-connections` | 32 | Live accepted capabilities, globally; bounded by the current CSpace partition |

Each TCP slot requests 8 KiB RX + 8 KiB TX, so the default buffer budget permits
up to 256 slots, **not 256 live application handles**. Metadata/allocator/kernel
overhead also belongs to netd's separate 32-MiB RAM account. Allocation is
fallible; exhaustion does not switch to control storage. The HTTP event loop
itself admits up to 32 live handles. Enlarging that bound beyond 32 requires a
larger capability partition, not just another numeric configuration value.

The root's bounded, self-RPC-free bootstrap heap is 8 MiB to accommodate frame/
tree metadata and transient growth while the pool expands. This is the existing
root exception, not an allocation fallback for netd or the HTTP application.

smoltcp's next poll deadline arms an independent timerd alarm. Ethernet IRQs
and timer hints wake endpoint receive; there is no periodic network tick.
When TX is backpressured, immediate egress waits for Ethernet readiness rather
than repeatedly scheduling a zero-delay alarm.

The driver posts four stable RX buffers and one TX buffer. Reset acknowledgment
precedes queue release, outstanding share drain and buffer destruction. Failure
retains backing and faults. Existing devmgr/supervisor external-owner cleanup
and concrete dependency edges apply; there is no restart or reassignment.
The demo admits two independent root-bus/function-zero PCI devices, twelve
active DMA mappings per assignment and a bounded 256-slot shared device bank.

## Scope

This is a small IPv4/DHCP/TCP demonstration, not a general HTTP stack. Headers
are bounded to 1 KiB; concurrent streams are handled by an event loop with a
fixed response. The budgeted TCP/TIME_WAIT pool can still fill; this is not a
throughput server or a Linux-equivalent socket implementation.
There is no DNS service, TLS, IPv6, routing/NAT service, dynamic network-control
API, broad recovery or physical-hardware proof. The VM allocator remains the
ordinary global mimalloc path; control storage is not an application OOM fallback.
