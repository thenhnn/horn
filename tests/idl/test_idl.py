import subprocess
from pathlib import Path

import pytest
from horn_idl import (
    Borrow,
    Copy,
    DeclarationError,
    Move,
    compile_path,
    sel4,
)
from horn_idl.emit.c import emit as emit_c
from horn_idl.emit.rust import emit as emit_rust

ROOT = Path(__file__).resolve().parents[2]


def load_source(tmp_path, name, source):
    path = tmp_path / f"{name}.py"
    path.write_text(source)
    return compile_path(path)


def test_example_schema_has_stable_shape():
    schema = compile_path(ROOT / "examples/idl/schema.py")
    assert [interface.fqn for interface in schema.interfaces] == [
        "net.nhnn.horn.counter",
        "net.nhnn.horn.control",
    ]
    counter = schema.interfaces[0]
    assert [method.name for method in counter.methods] == ["add", "exchange"]
    assert counter.methods[0].label != counter.methods[1].label
    exchange = counter.methods[1]
    assert [(cap.path, cap.cap.mode) for cap in exchange.request_caps()] == [
        ("connection.service", Copy),
        ("connection.wake", Borrow),
    ]
    assert [(cap.path, cap.cap.mode) for cap in exchange.result_caps()] == [
        ("result", Move)
    ]
    assert schema.objects[0].python_name == "CounterService"
    assert schema.rings[0].name == "ConsoleTx"


def test_labels_are_deterministic_and_schema_local():
    schema = compile_path(ROOT / "examples/idl/schema.py")
    labels = [method.label for _, method in schema.methods()]
    assert len(labels) == len(set(labels))
    assert all(label != 0 for label in labels)
    again = compile_path(ROOT / "examples/idl/schema.py")
    assert labels == [method.label for _, method in again.methods()]


def test_rust_and_c_glue_reference_runtime():
    schema = compile_path(ROOT / "examples/idl/schema.py")
    rust = emit_rust(schema)
    assert "use horn_ipc::*;" in rust
    assert "pub const COUNTER_ADD_LABEL" in rust
    assert "pub struct CounterTag;" in rust
    assert "pub struct CounterServiceTag;" in rust
    assert "pub fn counter_service_dispatch" in rust
    assert "pub fn counter_service_request_layout" in rust
    assert "pub fn counter_service_recv<S: AsyncServer>" in rust
    assert "pub fn counter_service_reply<S: CapSpace, R: AsyncReply>" in rust
    assert "pub fn counter_service_serve_one" not in rust
    assert "recv_request_dynamic" in rust
    assert "horn_tag!" not in rust
    header, source = emit_c(schema)
    assert "#include <horn_ipc.h>" in header
    assert "horn_counter_add" in header
    assert "horn_counter_service_dispatch" in header
    assert "void horn_encoder_u64(" not in source


def test_receiver_views_forward_borrow_move_and_oneway_without_new_wire(tmp_path):
    schema = load_source(
        tmp_path,
        "receiver_views",
        """
from horn_idl import Cap, Copy, Borrow, Move, U64, Result, iface, method, sel4
@iface("test.receiver")
class Sender:
    @method
    def call(self, transport: U64, target: U64,
             moved: Cap[sel4.Endpoint, Move],
             borrowed: Cap[sel4.Notification, Borrow],
             copied: Cap[sel4.Notification, Copy]) -> Result[U64, U64]: ...
    @method(oneway=True)
    def notify(self, transport: U64) -> None: ...
""",
    )
    rust = emit_rust(schema)
    view = rust[rust.index("pub struct SenderReceiver") :]
    assert "impl horn_ipc::RpcInterface for SenderReceiver" in view
    assert "type Tag = SenderTag;" in view
    assert "moved: Cap<Endpoint>" in view
    assert "borrowed: &Cap<Notification>" in view
    assert "copied: &Cap<Notification>" in view
    assert "Result<u64, CallError<u64>>" in view
    assert ".call(transport, target, moved, borrowed, copied)" in view
    assert ".notify(transport)" in view
    assert "SenderClient::new(__horn_transport, self.target)" in view
    assert "CapPacker" not in view
    assert "impl Drop for SenderReceiver" not in view


def test_c_parameters_do_not_collide_with_client_or_vtable_locals(tmp_path):
    schema = load_source(
        tmp_path,
        "colliding_parameters",
        """
from horn_idl import U32, iface, method
@iface("test.locals")
class LocalNames:
    @method
    def call(self, status: U32, ctx: U32, target: U32, result: U32,
             transport: U32, args: U32, arg_status: U32) -> U32: ...
""",
    )
    header, source = emit_c(schema)
    (tmp_path / "protocol.h").write_text(header)
    (tmp_path / "protocol.c").write_text(source)
    subprocess.run(
        [
            "cc",
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-I",
            str(ROOT / "platform/runtime/c/horn-ipc/include"),
            "-c",
            str(tmp_path / "protocol.c"),
            "-o",
            str(tmp_path / "protocol.o"),
        ],
        check=True,
        capture_output=True,
    )


def test_cap_target_must_be_capability_backed(tmp_path):
    with pytest.raises(DeclarationError, match="not a capability-backed type"):
        load_source(
            tmp_path,
            "bad_cap",
            """
from horn_idl import U32, Cap, Copy, iface, method

@iface("test.bad")
class Bad:
    @method
    def use(self, value: Cap[U32, Copy]) -> None: ...
""",
        )


def test_borrow_requires_reply_boundary(tmp_path):
    with pytest.raises(DeclarationError, match="borrow needs a reply boundary"):
        load_source(
            tmp_path,
            "bad_borrow",
            """
from horn_idl import Cap, Borrow, iface, method, sel4

@iface("test.bad")
class Bad:
    @method(oneway=True)
    def use(self, value: Cap[sel4.Notification, Borrow]) -> None: ...
""",
        )


def test_many_caps_are_packed_into_a_cnode(tmp_path):
    schema = load_source(
        tmp_path,
        "many_caps",
        """
from horn_idl import Cap, Copy, iface, method, sel4

@iface("test.many")
class Many:
    @method
    def use(self, a: Cap[sel4.Endpoint, Copy], b: Cap[sel4.Endpoint, Copy],
            c: Cap[sel4.Endpoint, Copy], d: Cap[sel4.Endpoint, Copy]) -> None: ...
""",
    )
    layout = schema.interfaces[0].methods[0].request_layout()
    assert len(layout.direct) == 0
    assert layout.root_count == 1
    assert layout.cnode is not None
    assert layout.cnode.size_bits == 2
    assert [slot.slot for slot in layout.cnode.slots] == [0, 1, 2, 3]


def test_ring_elements_cannot_hold_caps(tmp_path):
    with pytest.raises(DeclarationError, match="cannot contain capabilities"):
        load_source(
            tmp_path,
            "bad_ring",
            """
from horn_idl import Cap, Copy, ring, sel4

@ring(element=Cap[sel4.Endpoint, Copy], capacity=8)
class Bad:
    pass
""",
        )


def test_ring_and_region_record_elements_rejected(tmp_path):
    for kind in ("ring", "region"):
        with pytest.raises(DeclarationError, match="cannot be records yet"):
            load_source(
                tmp_path,
                f"record_{kind}",
                f"""
from horn_idl import U32, {kind}, record

@record
class Item:
    value: U32

@{kind}(element=Item, capacity=8)
class Items:
    pass
""",
            )


def test_recursive_record_rejected(tmp_path):
    with pytest.raises(DeclarationError, match="recursive record"):
        load_source(
            tmp_path,
            "bad_record",
            """
from horn_idl import U32, record

@record
class Node:
    value: U32
    next: "Node"
""",
        )


def test_generated_c_compiles_against_runtime(tmp_path):
    import shutil
    import subprocess

    cc = shutil.which("cc")
    if cc is None:
        pytest.skip("no C compiler")
    schema = compile_path(ROOT / "examples/idl/schema.py")
    header, source = emit_c(schema)
    (tmp_path / "protocol.h").write_text(header)
    (tmp_path / "protocol.c").write_text(source)
    subprocess.run(
        [
            cc,
            "-std=c11",
            "-Wall",
            "-Werror",
            "-I",
            str(ROOT / "platform/runtime/c/horn-ipc/include"),
            "-I",
            str(tmp_path),
            "-fsyntax-only",
            str(tmp_path / "protocol.c"),
        ],
        check=True,
    )


def test_generated_rust_compiles_against_runtime(tmp_path):
    import shutil
    import subprocess

    cargo = shutil.which("cargo")
    if cargo is None:
        pytest.skip("no cargo")
    schema = compile_path(ROOT / "examples/idl/schema.py")
    (tmp_path / "src").mkdir()
    (tmp_path / "src/lib.rs").write_text("#![no_std]\npub mod protocol;\n")
    (tmp_path / "src/protocol.rs").write_text(emit_rust(schema))
    runtime = ROOT / "platform/runtime/rust/horn-ipc"
    (tmp_path / "Cargo.toml").write_text(
        "[package]\n"
        'name = "idl-check"\n'
        'version = "0.1.0"\n'
        'edition = "2024"\n'
        "\n"
        "[dependencies]\n"
        f'horn-ipc = {{ path = "{runtime}" }}\n'
        "\n"
        "[workspace]\n"
    )
    subprocess.run(
        [cargo, "check", "--quiet", "--offline"],
        cwd=tmp_path,
        check=True,
    )


def test_cnode_layout_uses_one_flat_cnode():
    from horn_idl import CapMode, CapType, Rights
    from horn_idl.layout import MAX_LAYOUT_CAPS, build_layout
    from horn_idl.types import CapPath

    def caps(count):
        cap = CapType(sel4.Endpoint, CapMode.COPY, Rights.ALL)
        return [CapPath(f"cap[{index}]", cap) for index in range(count)]

    direct = build_layout(caps(1))
    assert len(direct.direct) == 1
    assert direct.cnode is None

    one = build_layout(caps(MAX_LAYOUT_CAPS))
    assert one.root_count == 1
    assert one.cnode is not None
    # A flat CNode just large enough for the whole layout.
    assert one.cnode.size_bits == (MAX_LAYOUT_CAPS + 1).bit_length() - 1
    assert len(one.cnode.slots) == MAX_LAYOUT_CAPS

    with pytest.raises(ValueError, match="CNode limit"):
        build_layout(caps(MAX_LAYOUT_CAPS + 1))


def test_async_methods_generate_deferred_server_helpers(tmp_path):

    schema = load_source(
        tmp_path,
        "async_schema",
        """
from horn_idl import U32, iface, method, object
from horn_idl.declarations import Transport

@iface("test.async", version=(1, 0))
class Async:
    @method(transport=Transport.ASYNC)
    def work(self, value: U32) -> U32: ...

@object(implements=(Async,))
class AsyncObject:
    pass
""",
    )
    rust = emit_rust(schema)
    assert "pub fn async_work_recv_async<S: AsyncServer>" in rust
    assert "pub fn async_work_reply_async<S: CapSpace, R: AsyncReply>" in rust
    assert "let (_label, args, reply) = server.recv_request" in rust
    assert "reply.reply_wire(space, &AsyncWorkReply { value: *result }" in rust
    assert "result: &u32" in rust
    assert "reply.reply_error" not in rust
    assert "pub fn async_object_recv<S: AsyncServer>" in rust
    assert "pub fn async_object_reply<S: CapSpace, R: AsyncReply>" in rust
    assert "fn work(&mut self, value: u32) -> u32" in rust
    assert "-> AsyncObjectResponse" in rust
    assert "Async::work(handler, args.value))" in rust


def test_object_reply_skips_oneway_methods(tmp_path):
    schema = load_source(
        tmp_path,
        "oneway_object",
        """
from horn_idl import U32, iface, method, object

@iface("test.notify", version=(1, 0))
class Notify:
    @method(oneway=True)
    def ping(self, value: U32) -> None: ...

@object(implements=(Notify,))
class NotifyService:
    pass
""",
    )
    rust = emit_rust(schema)
    assert "NotifyServiceResponse::NotifyPing(_value) => Ok(())" in rust
    assert "fn ping(&mut self, value: u32) -> ();" in rust
    assert "pub fn notify_service_serve_one" not in rust


def test_typed_capset_generates_slot_layout(tmp_path):

    schema = load_source(
        tmp_path,
        "capset_schema",
        """
from horn_idl import Cap, Copy, Move, U32, capset, iface, method, object, sel4

@iface("test.service", version=(1, 0))
class Service:
    @method
    def ping(self) -> U32: ...

@object(implements=(Service,))
class ServiceObject:
    pass

@capset
class ServiceCaps:
    service: Cap[ServiceObject, Move]
    wake: Cap[sel4.Notification, Copy]
""",
    )
    assert len(schema.capsets) == 1
    assert schema.capsets[0].type.slots == (("service", 1), ("wake", 2))
    rust = emit_rust(schema)
    assert "pub struct ServiceCapsTag;" in rust
    assert "SERVICE_CAPS_SLOTS" in rust
    assert "SERVICE_CAPS_SERVICE_SLOT: u32 = 1" in rust
    assert "SERVICE_CAPS_WAKE_SLOT: u32 = 2" in rust
    assert "SERVICE_CAPS_SELF_SLOT: u32 = 0" in rust
    assert "SERVICE_CAPS_FIRST_FREE_SLOT: u32 = 3" in rust
    assert "install_self_cnode" in rust
    header, _ = emit_c(schema)
    assert "HORN_SERVICE_CAPS_SLOT_SERVICE 1u" in header
    assert "HORN_SERVICE_CAPS_SLOT_WAKE 2u" in header


def test_inline_parcel_and_region_field(tmp_path):
    schema = load_source(
        tmp_path,
        "parcel_schema",
        """
from horn_idl import Cap, Move, Parcel, U8, U64, iface, method, record, region

@region(element=U8, capacity=4096)
class Buffer:
    pass

@record
class BlockRequest:
    block: U64
    data: Cap[Buffer, Move]

@iface("test.parcel", version=(1, 0))
class ParcelService:
    @method
    def send(self, value: Parcel[BlockRequest]) -> Parcel[U64]: ...
""",
    )
    method = schema.interfaces[0].methods[0]
    assert [(cap.path, cap.cap.mode) for cap in method.request_caps()] == [
        ("value.data", Move)
    ]
    rust = emit_rust(schema)
    assert "Parcel<BlockRequest, false>" in rust
    header, source = emit_c(schema)
    assert "horn_BlockRequest_t" in header
    assert "horn_ring" not in source


def test_result_is_a_tagged_specialization():
    from horn_idl.types import ResultType, TaggedEnumType

    schema = compile_path(ROOT / "examples/idl/tagged.py")
    method = schema.interfaces[0].methods[1]
    assert isinstance(method.result, ResultType)
    assert isinstance(method.result, TaggedEnumType)
    assert [(name, tag) for name, tag, _ in method.result.variants] == [
        ("Ok", 0),
        ("Err", 1),
    ]
    assert method.result.caps == 4
    assert method.result_layout().variable
    assert method.result_layout().cnode.slot_count == 4
    rust = emit_rust(schema)
    assert "-> Result<Event, Event>;" in rust
    assert "-> Result<Event, CallError<Event>>" in rust
    assert "reply.value.map_err(CallError::Remote)" in rust
    header, _ = emit_c(schema)
    assert "horn_probe_try_echo_result_t;" in header


@pytest.mark.parametrize("annotation", ["Result[U32, U32]", "Result[None, U32]"])
def test_oneway_cannot_return_result(tmp_path, annotation):
    with pytest.raises(DeclarationError, match="one-way methods cannot return"):
        load_source(
            tmp_path,
            "oneway_result",
            f"""
from horn_idl import U32, Result, iface, method
@iface("test.oneway_result")
class Test:
    @method(oneway=True)
    def run(self) -> {annotation}: ...
""",
        )


def test_tagged_enum_payloads_and_maximum_cap_budget():
    schema = compile_path(ROOT / "examples/idl/tagged.py")
    event = next(item.type for item in schema.enums if item.name == "Event")
    assert event.caps == 4
    assert event.payload_offset == 8
    assert event.size == 64
    assert [(name, tag) for name, tag, _ in event.variants][:3] == [
        ("Empty", 0),
        ("Number", 1),
        ("Data", 2),
    ]
    method = schema.interfaces[0].methods[0]
    layout = method.result_layout()
    assert layout.variable
    assert layout.cnode is not None
    assert layout.cnode.slot_count == 4
    rust = emit_rust(schema)
    assert "pub enum Event" in rust
    assert "Data(Payload)" in rust
    assert "finish_zeroed(Self::SIZE)" in rust
    header, source = emit_c(schema)
    assert "union {" in header
    assert "switch (value->tag)" in source
    assert "RESULT_CAP_COUNT 4u" in header


@pytest.mark.parametrize(
    "variant, message",
    [
        (
            "@enum.value(1)\\n    def A(): ...\\n    @enum.value(1)\\n    def B(): ...",
            "duplicate tag",
        ),
        ("@enum.value(-1)\\n    def A(): ...", "out of range"),
        ("@enum.value(1 << 32)\\n    def A(): ...", "out of range"),
        ("@enum.value(type='Event')\\n    def A(): ...", "recursive tagged enum"),
        ("@enum.value()\\n    def A(value): ...", "take no arguments"),
    ],
)
def test_invalid_tagged_enum(tmp_path, variant, message):
    with pytest.raises(DeclarationError, match=message):
        source = (
            "from horn_idl import enum\n@enum\nclass Event:\n    "
            + variant.replace("\\n", "\n")
            + "\n"
        )
        load_source(tmp_path, "bad_tagged", source)


def test_tagged_enum_implicit_tags_and_forward_payload(tmp_path):
    schema = load_source(
        tmp_path,
        "tagged_forward",
        """
from horn_idl import U32, enum, record
@enum
class Event:
    @enum.value(type="Payload")
    def Data(): ...
    @enum.value()
    def Empty(): ...
@record
class Payload:
    value: U32
""",
    )
    assert [(name, tag) for name, tag, _ in schema.enums[0].type.variants] == [
        ("Data", 0),
        ("Empty", 1),
    ]


def test_opaque_cnode_bulk_transfer(tmp_path):
    schema = load_source(
        tmp_path,
        "opaque_cnode",
        """
from horn_idl import Cap, Copy, Move, U32, iface, method, record, sel4

@record
class Bootstrap:
    cspace: Cap[sel4.CNode, Move]
    image: Cap[sel4.Frame, Copy]

@iface("test.spawn", version=(1, 0))
class Spawn:
    @method
    def start(self, bootstrap: Bootstrap) -> Cap[sel4.TCB, Move]: ...
""",
    )
    method = schema.interfaces[0].methods[0]
    assert [(cap.path, cap.cap.mode) for cap in method.request_caps()] == [
        ("bootstrap.cspace", Move),
        ("bootstrap.image", Copy),
    ]
    layout = method.request_layout()
    assert len(layout.direct) == 0
    assert layout.root_count == 1
    rust = emit_rust(schema)
    assert "Cap<CNode>" in rust


def test_capset_can_reserve_import_capacity(tmp_path):
    schema = load_source(
        tmp_path,
        "reserved_slots",
        """
from horn_idl import Cap, Copy, Move, capset, sel4, iface, method
@capset(capacity=256)
class Handles:
    wake: Cap[sel4.Notification, Copy]
@iface("test.bootstrap", version=(1, 0))
class Bootstrap:
    @method
    def install(self, handles: Cap[Handles, Move]) -> None: ...
""",
    )
    rust = emit_rust(schema)
    header, _ = emit_c(schema)
    assert "HANDLES_SIZE_BITS: u8 = 8" in rust
    assert "HANDLES_FIRST_FREE_SLOT: u32 = 2" in rust
    assert "HORN_HANDLES_SIZE_BITS 8u" in header


@pytest.mark.parametrize("capacity", [0, 1, 3, 1024])
def test_capset_rejects_invalid_capacity(tmp_path, capacity):
    with pytest.raises(DeclarationError):
        load_source(
            tmp_path,
            "invalid_slots",
            f"""
from horn_idl import Cap, Copy, capset, sel4
@capset(capacity={capacity})
class Handles:
    wake: Cap[sel4.Notification, Copy]
""",
        )


def test_vm_schema_has_bounded_batches_and_scoped_faults():
    schema = compile_path(ROOT / "platform/core/vm/idl/horn_vm.py")
    interfaces = {interface.python_name: interface for interface in schema.interfaces}
    assert set(interfaces) == {
        "VmMemory",
        "VmAddressSpace",
        "VmRegion",
        "VmPager",
        "VmFactory",
        "VmUntyped",
    }
    assert all(
        interface.fqn.startswith("net.nhnn.horn.vm.") for interface in schema.interfaces
    )
    methods = {
        (interface.python_name, method.name): method
        for interface, method in schema.methods()
    }
    for key in [("VmRegion", "apply"), ("VmPager", "resolve")]:
        method = methods[key]
        parcel = method.params[1].type
        assert parcel.inner.capacity == 16
        assert parcel.size <= 960
        layout = method.request_layout()
        assert layout.variable
        assert layout.cnode is None
        assert len(layout.direct) == 1
        assert all(slot.cap.mode is Borrow for slot in method.request_caps())
        assert method.result.error.name == "VmBatchError"
    drain = methods["VmPager", "drain"]
    assert drain.result_caps() == []
    assert drain.result.ok.inner.capacity == 16
    errors = next(value for value in schema.enums if value.name == "VmError")
    assert ("Unsupported", 8) in errors.variants
    assert ("Quarantined", 10) in errors.variants


def test_object_dispatch_keeps_interface_qualified_method_names():
    schema = compile_path(ROOT / "platform/core/vm/idl/horn_vm.py")
    host = next(value for value in schema.objects if value.python_name == "VmHost")
    assert len(list(host.methods())) == 22
    rust = emit_rust(schema)
    for interface in ("VmFactory", "VmMemory", "VmRegion"):
        assert f"{interface}::info(handler)" in rust
    header, _ = emit_c(schema)
    for interface in ("vm_factory", "vm_memory", "vm_region"):
        assert f"horn_{interface}_vtable_t {interface};" in header


def test_object_rejects_repeated_interface_method(tmp_path):
    with pytest.raises(DeclarationError, match="duplicate method"):
        load_source(
            tmp_path,
            "repeated_interface",
            """
from horn_idl import iface, method, object
@iface("test.repeat")
class Service:
    @method
    def ping(self) -> None: ...
@object(implements=(Service, Service))
class Host:
    pass
""",
        )
