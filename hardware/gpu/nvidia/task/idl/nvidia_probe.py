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


@enum(base=U32)
class VbiosState:
    SkippedGfw = 0
    FwsecParsed = 1
    Rejected = 2


@record
class ProbeSnapshot:
    boot0: U32
    boot42: U32
    gfw_state: GfwState
    # Unknown (0xffffffff) when privilege-locked.
    gfw_progress: U32
    bus_master: BOOL
    gsp_booted: BOOL
    # Parsing is not GPU authentication or execution. Fields valid only when parsed.
    vbios_state: VbiosState
    fwsec_bytes: U32
    fwsec_engines: U32
    fwsec_ucode: U32
    fwsec_signature_versions: U32


@iface("net.nhnn.horn.nvidia.probe", version=(1, 0))
class NvidiaProbe:
    @method
    def snapshot(self) -> Result[ProbeSnapshot, ProbeError]: ...


@object(implements=(NvidiaProbe,))
class NvidiaProbeObject:
    pass
