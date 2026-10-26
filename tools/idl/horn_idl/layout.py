"""Capability layout planning.

Ordinary seL4 IPC receives one capability directly.  When a
message has more, the generator transfers one flat CNode containing the caps.
There are no nested CNodes and no multi-root trees: the layout is static per
method, so the wire only needs the root CNode capability.
"""

from __future__ import annotations

from dataclasses import dataclass

from .types import CapMode, CapPath, CapType, Rights

DEFAULT_FANOUT = 256
MAX_LAYOUT_CAPS = DEFAULT_FANOUT
# Slot 0 of a capset CNode holds the self-CNode capability.
MAX_CAPSET_CAPS = MAX_LAYOUT_CAPS - 1


@dataclass(frozen=True)
class CapSlotLayout:
    path: str
    slot: int
    mode: CapMode
    rights: Rights


@dataclass(frozen=True)
class CapNodeLayout:
    size_bits: int
    slots: tuple[CapSlotLayout, ...]

    @property
    def slot_count(self) -> int:
        return len(self.slots)

    @property
    def capacity(self) -> int:
        return 1 << self.size_bits


@dataclass(frozen=True)
class CapLayout:
    direct: tuple[CapSlotLayout, ...]
    cnode: CapNodeLayout | None = None
    variable: bool = False

    @property
    def root_count(self) -> int:
        return 0 if self.cnode is None else 1


def _size_bits(count: int) -> int:
    if count <= 1:
        return 0
    return (count - 1).bit_length()


def build_layout(caps: list[CapPath], *, variable: bool = False) -> CapLayout:
    """Choose direct caps or one flat CNode."""

    if len(caps) > MAX_LAYOUT_CAPS:
        raise ValueError(
            f"{len(caps)} capabilities exceed the {MAX_LAYOUT_CAPS}-cap CNode limit"
        )
    if len(caps) <= 1:
        return CapLayout(
            direct=tuple(
                CapSlotLayout(
                    path=cap.path,
                    slot=index,
                    mode=cap.cap.mode,
                    rights=cap.cap.rights,
                )
                for index, cap in enumerate(caps)
            ),
            cnode=None,
            variable=variable,
        )
    slots = tuple(
        CapSlotLayout(
            path=cap.path,
            slot=index,
            mode=cap.cap.mode,
            rights=cap.cap.rights,
        )
        for index, cap in enumerate(caps)
    )
    return CapLayout(
        direct=(), cnode=CapNodeLayout(_size_bits(len(slots)), slots), variable=variable
    )


def build_capset_layout(fields) -> CapLayout:
    """A capset is always one flat CNode, even when it has <= 3 caps."""

    caps = [CapPath(field.name, field.type) for field in fields]
    if len(caps) != len(fields) or any(
        not isinstance(field.type, CapType) for field in fields
    ):
        raise TypeError("capset fields must be Cap[...]")
    if not caps:
        return CapLayout(direct=(), cnode=None)
    if len(caps) > MAX_CAPSET_CAPS:
        raise ValueError(
            f"{len(caps)} capset fields exceed the {MAX_CAPSET_CAPS}-cap CNode limit"
        )
    slots = tuple(
        CapSlotLayout(
            path=cap.path,
            slot=index + 1,
            mode=cap.cap.mode,
            rights=cap.cap.rights,
        )
        for index, cap in enumerate(caps)
    )
    return CapLayout(
        direct=(),
        cnode=CapNodeLayout(_size_bits(len(caps) + 1), slots),
    )
