import pytest


@pytest.mark.parametrize("cpus", [1, 2], ids=["up", "smp"])
@pytest.mark.parametrize("release", [False, True], ids=["debug", "release"])
@pytest.mark.parametrize("fault", [False, True], ids=["discovery", "fatal"])
def test_devmgr_discovery(system, release, fault, cpus):
    modules = ("examples/devmgr/module.nix",)
    if fault:
        modules += ("tests/boot/devmgr_fault.nix",)
    if release:
        modules += ("tests/boot/rust_release.nix",)
    vm = system(
        modules=modules,
        cpus=cpus,
        extra_args=("-device", "virtio-serial-pci", "-device", "virtio-net-pci"),
    )
    vm.start()
    if fault:
        assert vm.process.wait(timeout=60) == 0
        output = vm.serial.path.read_text()
        assert "DEVMGR_ALLOCATOR allocator=mimalloc" in output
        assert "DEVMGR_RPC_PASS" in output
        assert "DEVMGR_FATAL reboot=true reason=devmgr stopped or faulted" in output
        return
    vm.serial.wait_for(
        b"DEVMGR_BANK_PASS retained=true shared=true coalesced=true mmio_charge=0",
        timeout=60,
    )
    vm.serial.wait_for(b"DEVMGR_ALLOCATOR allocator=mimalloc")
    vm.serial.wait_for(b"DEVMGR_READY devices=", timeout=60)
    vm.serial.wait_for(b"DEVMGR_PLATFORM_PASS bounded=true returned=true")
    vm.serial.wait_for(b"DEVMGR_AUTH_PASS observer_rebadge=false")
    vm.serial.wait_for(
        b"DEVMGR_RPC_PASS ecam=true pagination=true info=true msi=true readonly=true",
        timeout=60,
    )
    assert vm.monitor.execute("query-status")["running"]
    vm.kill()


def test_device_manager_startup_deadline(system):
    vm = system(
        modules=("examples/devmgr/module.nix", "tests/boot/devmgr_stall.nix"),
        cpus=1,
    )
    vm.start()
    assert vm.process.wait(timeout=60) == 0
    output = vm.serial.path.read_text()
    assert "DEVMGR_FATAL reboot=true reason=startup timeout" in output
    assert "DEVMGR_READY" not in output
