"""Built-in seL4 capability targets.

These are not wire values.  They are the object types that a ``Cap[...]`` may
name.  The generator maps them to ``sel4::cap_type::*`` in Rust and to the
corresponding ``seL4_CPtr`` plus object type in C.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from .types import Rights


@dataclass(frozen=True)
class KernelResource:
    fqn: str
    rust_name: str
    c_name: str
    kernel_type: str
    rights: Rights = Rights.ALL

    @property
    def name(self) -> str:
        return self.fqn.rsplit(".", 1)[-1]


def _resource(name: str, kernel_type: str | None = None, *, rights: Rights = Rights.ALL) -> KernelResource:
    return KernelResource(
        fqn=f"sel4.{name}",
        rust_name=name,
        c_name=f"seL4_{name}",
        kernel_type=kernel_type or name,
        rights=rights,
    )


Signal = _resource("Signal", "Notification")
Notification = _resource("Notification")
Endpoint = _resource("Endpoint")
Reply = _resource("Reply")
TCB = _resource("TCB")
CNode = _resource("CNode")
Untyped = _resource("Untyped")
IRQHandler = _resource("IRQHandler")
IRQControl = _resource("IRQControl")
ASIDControl = _resource("ASIDControl")
ASIDPool = _resource("ASIDPool")
DomainSet = _resource("DomainSet")
VSpace = _resource("VSpace")
Frame = _resource("Frame")
Page = _resource("Page")
PageTable = _resource("PageTable")
AnyCap = _resource("AnyCap")


class Arch:
    def __init__(self, fqn: str, rust_name: str, c_name: str) -> None:
        self.fqn = fqn
        self.rust_name = rust_name
        self.c_name = c_name


x86 = Arch("sel4.x86", "x86", "x86")
arm = Arch("sel4.arm", "arm", "arm")
riscv = Arch("sel4.riscv", "riscv", "riscv")

# Architecture-specific object types are represented as resources too, but
# their Rust path is nested under the architecture module.
X86Page = KernelResource("sel4.x86.Page", "Page", "Page", "Page")
X86LargePage = KernelResource("sel4.x86.LargePage", "LargePage", "LargePage", "LargePage")
X86PML4 = KernelResource("sel4.x86.PML4", "PML4", "PML4", "PML4")
X86PDPT = KernelResource("sel4.x86.PDPT", "PDPT", "PDPT", "PDPT")
X86PageDirectory = KernelResource("sel4.x86.PageDirectory", "PageDirectory", "PageDirectory", "PageDirectory")
X86PageTable = KernelResource("sel4.x86.PageTable", "PageTable", "PageTable", "PageTable")
X86IOPort = KernelResource("sel4.x86.IOPort", "IOPort", "IOPort", "IOPort")
ARM_Page = KernelResource("sel4.arm.Page", "Page", "Page", "Page")

# Attach architecture namespaces for ``sel4.x86.Page``-style access.
x86.Page = X86Page
x86.LargePage = X86LargePage
x86.PML4 = X86PML4
x86.PDPT = X86PDPT
x86.PageDirectory = X86PageDirectory
x86.PageTable = X86PageTable
x86.IOPort = X86IOPort
arm.Page = ARM_Page

ALL = (
    Signal,
    Notification,
    Endpoint,
    Reply,
    TCB,
    CNode,
    Untyped,
    IRQHandler,
    IRQControl,
    ASIDControl,
    ASIDPool,
    DomainSet,
    VSpace,
    Frame,
    Page,
    PageTable,
    AnyCap,
    X86Page,
    X86LargePage,
    X86PML4,
    X86PDPT,
    X86PageDirectory,
    X86PageTable,
    X86IOPort,
    ARM_Page,
)

__all__ = [
    "ALL",
    "AnyCap",
    "ASIDControl",
    "ASIDPool",
    "ARM_Page",
    "CNode",
    "DomainSet",
    "Endpoint",
    "Frame",
    "IRQControl",
    "IRQHandler",
    "KernelResource",
    "Notification",
    "Page",
    "PageTable",
    "Reply",
    "Signal",
    "TCB",
    "Untyped",
    "VSpace",
    "X86IOPort",
    "X86LargePage",
    "X86PDPT",
    "X86Page",
    "X86PageDirectory",
    "X86PageTable",
    "X86PML4",
    "arm",
    "riscv",
    "x86",
]
