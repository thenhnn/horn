import pytest


@pytest.mark.parametrize("release", [False, True], ids=["debug", "release"])
def test_idl_child_process_demo(system, release):
    modules = ("examples/idl/demo/module.nix",)
    if release:
        modules += ("tests/boot/rust_release.nix",)
    vm = system(modules=modules)
    vm.start()
    vm.serial.wait_for(b"MEMORY_BACKING_PASS zeroed=true rollback=true reclaimed=true")
    vm.serial.wait_for(b"IDL_DEMO_PASS value=41")
    vm.serial.wait_for(
        b"MEMORY_RPC_PASS allocation=true errors=true isolation=true", timeout=15
    )
    assert vm.monitor.execute("query-status")["running"]
    vm.kill()
