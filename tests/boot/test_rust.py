import pytest


@pytest.mark.parametrize("release", [False, True], ids=["debug", "release"])
def test_rust_root_task_boots(system, release):
    modules = ("tests/boot/rust_root_task.nix",)
    if release:
        modules += ("tests/boot/rust_release.nix",)
    vm = system(modules=modules)
    vm.start()
    vm.serial.wait_for(b"hi im rust root task")
    assert vm.monitor.execute("query-status")["running"]
    vm.kill()
