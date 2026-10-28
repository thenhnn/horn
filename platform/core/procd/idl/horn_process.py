"""Scoped launch/observation, per-process control, and child-only lifecycle."""

from horn_idl import (
    U32,
    U64,
    Borrow,
    Cap,
    Copy,
    List,
    Move,
    Option,
    Result,
    String,
    capset,
    enum,
    iface,
    method,
    object,
    record,
    sel4,
)

# Dependencies of the imported object must also be visible to the IDL collector.
from horn_vm import (  # noqa: F401
    VmError,
    VmMemory,
    VmMemoryInfo,
    VmMemoryObject,
    VmProtection,
    VmSeals,
)


@enum(base=U32)
class ProcessError:
    Denied = 1
    State = 2
    Backend = 3
    Invalid = 4
    InvalidElf = 5
    InvalidAbi = 6
    Exhausted = 7
    QuotaExceeded = 8
    Quarantined = 9
    Stale = 10


@record
class ProcessUsage:
    limit_bytes: U64
    charged_bytes: U64
    quarantined_bytes: U64


@iface("net.nhnn.horn.process.lifecycle", version=(1, 0))
class ProcessLifecycle:
    @method
    def info(self) -> Result[ProcessUsage, ProcessError]: ...

    # Successful exit stops the caller; no reply is delivered.
    @method
    def exit(self, status: U32) -> Result[None, ProcessError]: ...


@object(implements=(ProcessLifecycle,))
class ProcessSelf:
    pass


@enum(base=U32)
class ProcessState:
    Created = 1
    Active = 2
    Stopping = 3
    Exited = 4
    Quarantined = 5


@enum(base=U32)
class ProcessExit:
    @enum.value(0, type=U32)
    def Code(): ...

    @enum.value(1)
    def Killed(): ...


@record
class ProcessInfo:
    id: U64
    name: String[64]
    state: ProcessState
    exit: Option[ProcessExit]
    ram: ProcessUsage


@record
class ProcessPage:
    entries: List[ProcessInfo, 4]
    next: U64


@record
class ProcessLaunch:
    name: String[64]
    ram_limit: U64


@enum(base=U32)
class ProcessKernelObject:
    Endpoint = 1
    Notification = 2


@record
class ProcessBootstrap:
    vm: U32
    address_space: U32
    lifecycle: U32
    reply: U32


@iface("net.nhnn.horn.process.control", version=(1, 0))
class ProcessControl:
    @method
    def info(self) -> Result[ProcessInfo, ProcessError]: ...

    @method
    def start(self) -> Result[None, ProcessError]: ...

    @method
    def set_limit(self, limit: U64) -> Result[None, ProcessError]: ...

    # Stop execution without reclaiming VM/backing; device revocation comes next.
    @method
    def suspend(self) -> Result[None, ProcessError]: ...

    @method
    def kernel_object(
        self, kind: ProcessKernelObject
    ) -> Result[Cap[sel4.AnyCap, Move], ProcessError]: ...

    # Only Created children. Source authority is the borrowed CNode, not its index.
    @method
    def install(
        self,
        source: Cap[sel4.CNode, Borrow],
        source_bits: U32,
        source_slot: U32,
        target_slot: U32,
        badge: Option[U64],
        rights: U32,
    ) -> Result[None, ProcessError]: ...

    @method
    def fault_handler(
        self, source: Cap[sel4.CNode, Borrow], source_bits: U32, source_slot: U32
    ) -> Result[None, ProcessError]: ...

    @method
    def exit_signal(
        self, notification: Cap[sel4.Notification, Borrow]
    ) -> Result[None, ProcessError]: ...

    @method
    def bind_notification(
        self, notification: Cap[sel4.Notification, Borrow]
    ) -> Result[None, ProcessError]: ...

    @method
    def terminate(self) -> Result[None, ProcessError]: ...

    # Retire an exited record and revoke every copy of its control capability.
    @method
    def close(self) -> Result[None, ProcessError]: ...


@object(implements=(ProcessControl,))
class ProcessObject:
    pass


@iface("net.nhnn.horn.process.factory", version=(1, 0))
class ProcessFactory:
    @method
    def launch(
        self, elf: Cap[VmMemoryObject, Borrow], request: ProcessLaunch
    ) -> Result[Cap[ProcessObject, Move], ProcessError]: ...

    @method
    def prepare(
        self,
        elf: Cap[VmMemoryObject, Borrow],
        request: ProcessLaunch,
        bootstrap: ProcessBootstrap,
    ) -> Result[Cap[ProcessObject, Move], ProcessError]: ...

    # The factory already controls its own children; IDs select within that scope.
    @method
    def watch(
        self, notification: Cap[sel4.Notification, Borrow]
    ) -> Result[None, ProcessError]: ...

    @method
    def open(self, id: U64) -> Result[Cap[ProcessObject, Move], ProcessError]: ...


@iface("net.nhnn.horn.process.observe", version=(1, 0))
class ProcessObserver:
    @method
    def list(self, after: U64) -> Result[ProcessPage, ProcessError]: ...


@object(implements=(ProcessLifecycle, ProcessFactory, ProcessObserver, ProcessControl))
class ProcessHost:
    pass


@capset(capacity=256)
class ProcessCaps:
    vm: Cap[sel4.Endpoint, Copy]
    address_space: Cap[sel4.Endpoint, Copy]
    lifecycle: Cap[sel4.Endpoint, Copy]
    reply: Cap[sel4.Reply, Move]
