"""The child gets only its own lifecycle capability, not parent control."""

from horn_idl import U32, U64, Result, enum, iface, method, object, record


@enum(base=U32)
class ProcessError:
    Denied = 1
    State = 2
    Backend = 3


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
