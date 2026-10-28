import pytest


@pytest.mark.parametrize("release", [False, True], ids=["debug", "release"])
def test_idl_child_process_demo(system, release):
    modules = ("examples/idl/demo/module.nix",)
    if release:
        modules += ("tests/boot/rust_release.nix",)
    vm = system(modules=modules, cpus=2)
    vm.start()
    vm.serial.wait_for(b"VM_SERVICE_CPU id=0", timeout=60)
    vm.serial.wait_for(
        b"MEMORY_BACKING_PASS zeroed=true rollback=true reclaimed=true", timeout=60
    )
    vm.serial.wait_for(b"NX_EXECUTE_NEVER_PASS fault=true")
    vm.serial.wait_for(b"IDL_DEMO_PASS value=41")
    vm.serial.wait_for(b"LAUNCH_CHILD_PASS bootstrap=true")
    vm.serial.wait_for(
        b"LAUNCH_RPC_PASS sealed=true quota=true pagination=true control=true cleanup=true"
    )
    vm.serial.wait_for(b"VM_CLIENT_CPU id=1")
    vm.serial.wait_for(
        b"VM_SEALS_PASS aliases=true future_write=true immutable=true cleanup=true"
    )
    vm.serial.wait_for(
        b"VM_RPC_PASS mapping=true sharing=true lifetime=true protection=true errors=true",
        timeout=30,
    )
    vm.serial.wait_for(
        b"MIMALLOC_VM_PASS allocation=true alignment=true exhaustion=true pages_returned=true",
        timeout=60,
    )
    vm.serial.wait_for(
        b"MIMALLOC_GLOBAL_PASS vec=true box=true realloc=true zeroed=true alignment=true bootstrap_free=true exhaustion=true pages_returned=true",
        timeout=60,
    )
    vm.serial.wait_for(
        b"PROCD_PASS elf=true quota=true untyped=true cleanup=true", timeout=30
    )
    assert vm.monitor.execute("query-status")["running"]
    vm.kill()
