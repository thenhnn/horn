# Capability-isolated HTTP demo

Run the existing console/shell demo:

```sh
nix-shell --run 'just shell'       # KVM; explicit fallback: just shell tcg
curl --noproxy '*' http://127.0.0.1:8080/
# Hello from Horn!
```

QEMU user networking forwards **127.0.0.1:8080 → guest TCP 80**, without TAP or
root. `restrict=on` limits guest networking; the forward binds localhost only.
Port 8080 must be free. Exit the console with **Ctrl-A, then x**.
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
   a notification. It reserves port 80, reads bounded request headers, sends a
   fixed text response with Content-Length/Connection: close, then closes.
   There is no network-configuration or device authority in the application.

Nix definitions in `examples/devmgr/module.nix` wire dependencies and separate
RAM accounts. `examples/devmgr/console.nix` enables networking in `just shell`;
clock/timerd remain enabled without it. The PCI catalog matches modern
`1af4:1041`, not a fixed BDF. The example uses:
`/net/ethernet/virtio/0000:00:05.0`, `/services/clock`, `/services/net/tcp`.
As elsewhere, paths select published capabilities; paths do not confer authority.

## Bounded APIs and lifetime

Ethernet frames are 14–1514 bytes, transferred in at most 512-byte chunks.
Receive preserves a frame boundary; begin/append/commit stages one transmit
frame. RX/TX queues hold eight frames each. Generation watches are one-shot
hints, with recheck/rearm rather than polling. netd has bounded local queues.

A TCP factory reserves a port and returns a badged server capability. No
caller-supplied socket ID selects another reservation. A server has one
accepted connection at a time and eight TCP slots, including graceful closing
and TIME_WAIT, so closing one connection need not block listening. There are
four port reservations per netd lifetime; slots and port records are bounded,
not an unbounded socket API. Read/write parcels are 256 bytes with explicit
WouldBlock/Closed. Watches select ACCEPT/READABLE/WRITABLE/CLOSED interests;
writable sockets do not continuously wake readers. Idle TCP timeout is 30 s.

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
are bounded to 1 KiB; requests are handled sequentially with a fixed response.
The TCP/TIME_WAIT pool can fill under bursts; this is not a throughput server.
There is no DNS service, TLS, IPv6, routing/NAT service, dynamic network-control
API, broad recovery or physical-hardware proof. The VM allocator remains the
ordinary global mimalloc path; control storage is not an application OOM fallback.
