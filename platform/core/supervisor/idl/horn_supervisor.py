"""Process supervision. No PCI, driver names or device cleanup operations."""

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
    Result,
    capset,
    enum,
    iface,
    method,
    object,
    sel4,
)
from horn_log import *
from horn_process import *
from horn_registry import *
from horn_vm import *


@enum(base=U32)
class SystemError:
    Denied = 1


@enum(base=U32)
class SystemFailure:
    CriticalService = 1


@iface("net.nhnn.horn.supervisor.system", version=(1, 0))
class SystemControl:
    @method
    def ready(self) -> Result[None, SystemError]: ...

    @method
    def ticks_per_us(self) -> Result[U64, SystemError]: ...

    @method
    def fail(self, reason: SystemFailure) -> Result[None, SystemError]: ...


@object(implements=(SystemControl,))
class SystemHost:
    pass


@enum(base=U32)
class SupervisorError:
    Denied = 1
    NotFound = 2
    Busy = 3
    Exhausted = 4
    Invalid = 5
    Backend = 6


@enum(base=U32)
class StopMode:
    Graceful = 1
    Force = 2


@enum(base=U32)
class ServiceSignal:
    Ready = 1
    Stopped = 2


@enum(base=U32)
class ServiceStatus:
    Created = 1
    Starting = 2
    Active = 3
    Stopping = 4
    AwaitingCleanup = 5
    Exited = 6


@iface("net.nhnn.horn.supervisor.factory", version=(1, 0))
class SupervisorFactory:
    @method
    def prepare(
        self,
        elf: Cap[VmMemoryObject, Borrow],
        request: ProcessLaunch,
        bootstrap: ProcessBootstrap,
    ) -> Result[Cap[SupervisedJob, Move], SupervisorError]: ...


@iface("net.nhnn.horn.supervisor.job", version=(1, 0))
class SupervisorJob:
    @method
    def publish(
        self, path: List[U8, 96], endpoint: Cap[sel4.Endpoint, Borrow]
    ) -> Result[None, SupervisorError]: ...

    @method
    def client(self) -> Result[Cap[SupervisedClient, Move], SupervisorError]: ...

    @method
    def process(self) -> Result[Cap[ProcessObject, Move], SupervisorError]: ...

    @method
    def signal(
        self, kind: ServiceSignal
    ) -> Result[Cap[sel4.Notification, Move], SupervisorError]: ...

    @method
    def watch(
        self, owner: Cap[sel4.Notification, Borrow]
    ) -> Result[None, SupervisorError]: ...

    @method
    def stop_signal(
        self, stop: Cap[sel4.Notification, Borrow]
    ) -> Result[None, SupervisorError]: ...

    @method
    def log_sink(self) -> Result[Cap[LogSinkObject, Move], SupervisorError]: ...

    @method
    def start(self) -> Result[None, SupervisorError]: ...

    @method
    def status(self) -> Result[ServiceStatus, SupervisorError]: ...

    @method
    def cooperative(self) -> Result[BOOL, SupervisorError]: ...

    # Acknowledges intent; never synchronously calls back into the owner.
    @method
    def request_stop(self, mode: StopMode) -> Result[None, SupervisorError]: ...

    # Legal only after execution stopped and the owner completed external cleanup.
    @method
    def cleanup_done(self) -> Result[None, SupervisorError]: ...

    # Bind through a kernel-authenticated directory scope, recording the
    # exporting owner's lifetime dependency before child execution.
    @method
    def require(
        self, scope: Cap[RegistryNode, Borrow], path: List[U8, 96], slot: U32
    ) -> Result[None, SupervisorError]: ...


@object(implements=(SupervisorJob,))
class SupervisedJob:
    pass


@iface("net.nhnn.horn.supervisor.runtime", version=(1, 0))
class SupervisorRuntime:
    @method
    def status(self) -> Result[ServiceStatus, SupervisorError]: ...

    @method
    def request_stop(self, mode: StopMode) -> Result[None, SupervisorError]: ...


@object(implements=(SupervisorRuntime,))
class SupervisedClient:
    pass


@object(implements=(SupervisorFactory, SupervisorJob, SupervisorRuntime))
class SupervisorHost:
    pass


@capset(capacity=256)
class SupervisorCaps:
    vm: Cap[sel4.Endpoint, Copy]
    address_space: Cap[sel4.Endpoint, Copy]
    lifecycle: Cap[sel4.Endpoint, Copy]
    reply: Cap[sel4.Reply, Move]
    factory: Cap[sel4.Endpoint, Copy]
    platform: Cap[sel4.Endpoint, Copy]
    system: Cap[sel4.Endpoint, Copy]
    service: Cap[sel4.Endpoint, Copy]
    events: Cap[sel4.Notification, Copy]
    critical_fault: Cap[sel4.Endpoint, Copy]
    critical_exit: Cap[sel4.Notification, Copy]
    processes: Cap[sel4.Endpoint, Copy]
