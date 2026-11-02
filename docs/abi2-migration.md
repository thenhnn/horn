# ABI2 and shared logging migration

Migrate in buildable slices, without changing RPC protocols. Root task keeps
its native seL4 entry point. Keep direct output only for bootstrap, panic,
logger failures and critical contexts that cannot safely perform RPC.

| Component | Status |
| --- | --- |
| logd | ABI2, shared named-sink factory; no recursive logging grant |
| timerd | ABI2 and shared logger; user boot-confirmed |
| HTTP demo, shell | ABI2 and shared logger; user confirmed shell commands and HTTP |
| netd | ABI2, named caps, Inbox receive cleanup, DHCP logs; deployment builds, awaiting boot confirmation |
| virtio console/net drivers | Pending: devmgr owns dynamic driver startup, IRQ/stop and DMA cleanup |
| devmgr, supervisor | Pending: boot-critical startup and logger dependency ordering |
| NVIDIA | Pending source migration only; hardware launches remain separately authorized |
| legacy examples, probe fixtures | Pending inventory and migration |

Netd owns an explicit Context in its TCP service. Ethernet/time helpers receive
only their named capabilities; no ambient context, numeric bootstrap slots or
root-CNode accessor. Context revoke preserves the existing per-connection
anchor revocation before deletion. Notification badge conventions, coalescing,
alarm scheduling and TCP limits are unchanged.

The driver step must update the **launch path**, not merely change task entry
macros: devmgr currently installs DeviceDriverCaps and starts processes directly.
Do not introduce a boot cycle by synchronously waiting for logd inside devmgr's
initial driver launch. Preserve IRQ/stop acknowledgement and DMA teardown order.

Remove ABI1 macros, descriptors/capsets and ordinary debug printing only after
all consumers are migrated. Test matchers should follow readable messages
rather than retaining machine-marker prints solely for tests.

Validation policy: compile unit-test sources, do not execute unit tests. The user
runs QEMU for each agreed boot checkpoint; no agent QEMU or GPU launches.
