"""Capability layout planning.

The wire message can carry at most three capabilities directly.  When a
message has more, the generator builds a compact CNode layout: one to three
root CNodes are transferred, and each root may contain child CNodes.

The layout is static per method, so no tree metadata is needed on the wire.
"""

from __future__ import annotations

from dataclasses import dataclass

from .types import CapMode, CapPath, CapType, Rights

DEFAULT_FANOUT = 256
MAX_LAYOUT_CAPS = 4096


@dataclass(frozen=True)
class CapSlotLayout:
    path: str
    slot: int
    mode: CapMode
    rights: Rights


@dataclass(frozen=True)
class CapEntryLayout:
    slot: int
    leaf: CapSlotLayout | None = None
    child: CapNodeLayout | None = None


@dataclass(frozen=True)
class CapNodeLayout:
    size_bits: int
    entries: tuple[CapEntryLayout, ...]

    @property
    def slots(self) -> int:
        return 1 << self.size_bits


@dataclass(frozen=True)
class CapLayout:
    direct: tuple[CapSlotLayout, ...]
    roots: tuple[CapNodeLayout, ...]

    @property
    def root_count(self) -> int:
        return len(self.roots)


def _size_bits(count: int) -> int:
    if count <= 1:
        return 0
    return (count - 1).bit_length()


def _chunks(items: list[CapPath], size: int) -> list[list[CapPath]]:
    return [items[index : index + size] for index in range(0, len(items), size)]


def build_layout(caps: list[CapPath], fanout: int = DEFAULT_FANOUT, max_roots: int = 3) -> CapLayout:
    """Choose direct caps, one CNode, up to three CNodes, or a tree."""

    if len(caps) <= 3:
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
            roots=(),
        )

    nodes: list[CapNodeLayout] = []
    for group in _chunks(caps, fanout):
        entries = tuple(
            CapEntryLayout(
                slot=index,
                leaf=CapSlotLayout(
                    path=cap.path,
                    slot=index,
                    mode=cap.cap.mode,
                    rights=cap.cap.rights,
                ),
            )
            for index, cap in enumerate(group)
        )
        nodes.append(CapNodeLayout(_size_bits(len(group)), entries))

    while len(nodes) > max_roots:
        parents: list[CapNodeLayout] = []
        for group in _chunks(nodes, fanout):
            entries = tuple(
                CapEntryLayout(slot=index, child=child)
                for index, child in enumerate(group)
            )
            parents.append(CapNodeLayout(_size_bits(len(group)), entries))
        nodes = parents

    return CapLayout(direct=(), roots=tuple(nodes))


def build_capset_layout(fields, fanout: int = DEFAULT_FANOUT) -> CapLayout:
    """A capset is always a CNode layout, even when it has <= 3 caps."""

    caps = [CapPath(field.name, field.type) for field in fields]
    if len(caps) != len(fields) or any(not isinstance(field.type, CapType) for field in fields):
        raise TypeError("capset fields must be Cap[...]")
    if not caps:
        return CapLayout(direct=(), roots=())
    if len(caps) <= fanout:
        entries = tuple(
            CapEntryLayout(
                slot=index + 1,
                leaf=CapSlotLayout(
                    path=cap.path,
                    slot=index + 1,
                    mode=cap.cap.mode,
                    rights=cap.cap.rights,
                ),
            )
            for index, cap in enumerate(caps)
        )
        return CapLayout(
            direct=(),
            roots=(CapNodeLayout(_size_bits(len(caps) + 1), entries),),
        )
    layout = build_layout(caps, fanout=fanout, max_roots=1)
    root = layout.roots[0]
    shifted = CapNodeLayout(
        root.size_bits,
        tuple(
            CapEntryLayout(slot=entry.slot + 1, child=entry.child, leaf=entry.leaf)
            for entry in root.entries
        ),
    )
    return CapLayout(direct=(), roots=(shifted,))
