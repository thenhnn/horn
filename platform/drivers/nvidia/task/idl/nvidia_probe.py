"""Read-only probe snapshot. Not a GPU, rendering or GSP-ready interface."""

from horn_idl import *


@enum(base=U32)
class ProbeError:
    Stopped = 1


@enum(base=U32)
class GfwState:
    PrivilegeLocked = 0
    InProgress = 1
    Complete = 2


@record
class ProbeSnapshot:
    boot0: U32
    boot42: U32
    gfw_state: GfwState
    # Unknown (0xffffffff) when privilege-locked.
    gfw_progress: U32
    bus_master: BOOL
    gsp_booted: BOOL


@iface("net.nhnn.horn.nvidia.probe", version=(1, 0))
class NvidiaProbe:
    @method
    def snapshot(self) -> Result[ProbeSnapshot, ProbeError]: ...


@object(implements=(NvidiaProbe,))
class NvidiaProbeObject:
    pass
