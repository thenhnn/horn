"""Scoped launch/observation, per-process control, and child-only lifecycle."""

from horn_idl import (
    BOOL,
    U8,
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

# Imported VM objects and their transitive wire dependencies.
from horn_vm import *


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


@record
class ProcessPlacement:
    cpu: U32
    cpu_count: U32


@record
class ExecutionRegisters:
    ip: U64
    sp: U64
    flags: U64
    ax: U64
    bx: U64
    cx: U64
    dx: U64
    si: U64
    di: U64
    bp: U64
    r8: U64
    r9: U64
    r10: U64
    r11: U64
    r12: U64
    r13: U64
    r14: U64
    r15: U64
    fs: U64
    gs: U64


@enum(base=U32)
class ExecutionReason:
    Interrupted = 0
    Syscall = 1
    PageFault = 2
    Exception = 3
    Budget = 4


@record
class ExecutionExit:
    reason: ExecutionReason
    address: U64
    code: U64
    registers: ExecutionRegisters


@record
class ExecutionFpInfo:
    bytes: U32
    features: U64


@iface("net.nhnn.horn.process.lifecycle", version=(1, 0))
class ProcessLifecycle:
    # Read-only topology/initial placement, never scheduling authority.
    @method
    def placement(self) -> Result[ProcessPlacement, ProcessError]: ...

    @method
    def info(self) -> Result[ProcessUsage, ProcessError]: ...

    # Successful exit stops the caller; no reply is delivered.
    @method
    def exit(self, status: U32) -> Result[None, ProcessError]: ...


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
    # Explicitly delegate a scoped, target-funded child factory if requested.
    factory: Option[U32]
    vm: U32
    address_space: U32
    lifecycle: U32
    reply: U32


@record
class ProcessEmpty:
    launch: ProcessLaunch
    address: U64
    length: U64


@enum(base=U32)
class ThreadAbi:
    Native = 1
    TrappedSyscalls = 2


@enum(base=U32)
class ThreadState:
    Stopped = 1
    Running = 2
    Quarantined = 3


@record
class ThreadCreate:
    abi: ThreadAbi
    allocate_ipc_buffer: BOOL
    registers: ExecutionRegisters


@record
class ThreadInfo:
    id: U64
    state: ThreadState
    reason: Option[ExecutionReason]


@record
class ThreadPage:
    entries: List[ThreadInfo, 8]
    next: Option[U64]


@iface("net.nhnn.horn.thread.observe", version=(1, 0))
class ThreadObserver:
    @method
    def thread_info(self) -> Result[ThreadInfo, ProcessError]: ...


@object(implements=(ThreadObserver,))
class ThreadObserverObject:
    pass


@iface("net.nhnn.horn.thread.control", version=(1, 0))
class ThreadControl:
    # Full target authority, including execution-state inspection/mutation.
    @method
    def info(self) -> Result[ThreadInfo, ProcessError]: ...
    @method
    def observe(self) -> Result[Cap[ThreadObserverObject, Move], ProcessError]: ...
    @method
    def watch(
        self, events: Cap[sel4.Notification, Borrow]
    ) -> Result[None, ProcessError]: ...
    @method
    def registers(self) -> Result[ExecutionRegisters, ProcessError]: ...
    @method
    def set_registers(
        self, registers: ExecutionRegisters
    ) -> Result[None, ProcessError]: ...
    @method
    def ipc_buffer(self) -> Result[Option[U64], ProcessError]: ...
    @method
    def allocate_ipc_buffer(self) -> Result[U64, ProcessError]: ...
    @method
    def stop(self) -> Result[ExecutionExit, ProcessError]: ...
    @method
    def fp_info(self) -> Result[ExecutionFpInfo, ProcessError]: ...
    @method
    def fp_read(
        self, offset: U32, length: U32
    ) -> Result[List[U8, 256], ProcessError]: ...
    @method
    def fp_write(
        self, offset: U32, data: List[U8, 256], commit: BOOL
    ) -> Result[None, ProcessError]: ...
    @method
    def destroy(self) -> Result[None, ProcessError]: ...


@object(implements=(ThreadControl,))
class ThreadObject:
    pass


@iface("net.nhnn.horn.execution.context", version=(1, 0))
class ExecutionContext:
    @method
    def run(self, thread: Cap[ThreadObject, Borrow]) -> Result[None, ProcessError]: ...
    @method
    def stop(self) -> Result[None, ProcessError]: ...


@object(implements=(ExecutionContext,))
class ExecutionContextObject:
    pass


@iface("net.nhnn.horn.process.threads", version=(1, 0))
class ProcessThreads:
    @method
    def create_thread(
        self, request: ThreadCreate
    ) -> Result[Cap[ThreadObject, Move], ProcessError]: ...
    @method
    def list_threads(self, after: Option[U64]) -> Result[ThreadPage, ProcessError]: ...
    @method
    def open_thread(self, id: U64) -> Result[Cap[ThreadObject, Move], ProcessError]: ...
    @method
    def execution_context(
        self,
    ) -> Result[Cap[ExecutionContextObject, Move], ProcessError]: ...


@object(implements=(ProcessLifecycle, ProcessThreads))
class ProcessSelf:
    pass


@iface("net.nhnn.horn.process.view", version=(1, 0))
class ProcessView:
    @method
    def process_info(self) -> Result[ProcessInfo, ProcessError]: ...
    @method
    def list_threads(self, after: Option[U64]) -> Result[ThreadPage, ProcessError]: ...


@object(implements=(ProcessView,))
class ProcessObserverObject:
    pass


@iface("net.nhnn.horn.process.control", version=(2, 0))
class ProcessControl:
    @method
    def observe(self) -> Result[Cap[ProcessObserverObject, Move], ProcessError]: ...
    @method
    def vm(self) -> Result[Cap[VmService, Move], ProcessError]: ...
    @method
    def address_space(
        self,
    ) -> Result[Cap[VmAddressSpaceObject, Move], ProcessError]: ...

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

    # ABI2 only, Created child. Names/selectors in sealed resident memory;
    # runtime addresses are supplied by procd, not the provisioning descriptor.
    @method
    def startup_context(
        self, descriptor: Cap[VmMemoryObject, Borrow], length: U64
    ) -> Result[None, ProcessError]: ...


@object(implements=(ProcessControl, ProcessThreads))
class ProcessObject:
    pass


@iface("net.nhnn.horn.process.factory", version=(2, 0))
class ProcessFactory:
    @method
    def spawn(
        self,
        elf: Cap[VmMemoryObject, Borrow],
        request: ProcessLaunch,
        bootstrap: Option[ProcessBootstrap],
    ) -> Result[Cap[ProcessObject, Move], ProcessError]: ...

    @method
    def spawn_empty(
        self, request: ProcessEmpty
    ) -> Result[Cap[ProcessObject, Move], ProcessError]: ...

    # The factory already controls its own children; IDs select within that scope.
    @method
    def watch(
        self, notification: Cap[sel4.Notification, Borrow]
    ) -> Result[None, ProcessError]: ...

    @method
    def open(self, id: U64) -> Result[Cap[ProcessObject, Move], ProcessError]: ...


@object(implements=(ProcessFactory,))
class ProcessFactoryObject:
    pass


@iface("net.nhnn.horn.process.observe", version=(1, 0))
class ProcessObserver:
    @method
    def list(self, after: U64) -> Result[ProcessPage, ProcessError]: ...


@object(
    implements=(
        ProcessLifecycle,
        ProcessFactory,
        ProcessObserver,
        ProcessControl,
        ProcessThreads,
        ProcessView,
        ThreadControl,
        ThreadObserver,
        ExecutionContext,
    )
)
class ProcessHost:
    pass


@capset(capacity=256)
class ProcessCaps:
    vm: Cap[sel4.Endpoint, Copy]
    address_space: Cap[sel4.Endpoint, Copy]
    lifecycle: Cap[sel4.Endpoint, Copy]
    reply: Cap[sel4.Reply, Move]
