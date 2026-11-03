import pytest
from test_virtio_console import assert_idle, console_connection


@pytest.mark.parametrize("console", [False, True], ids=["discovery", "console"])
def test_devmgr_ordinary_boot_has_no_probes(system, console):
    modules = ("platform/system/module.nix",)
    if console:
        modules += ("tests/boot/virtio_console_clean.nix",)
    vm = system(modules=modules, cpus=1)
    if console:
        with console_connection(vm) as connection:
            greeting = b"\x1b[1;36mHorn v0.1 started\x1b[0m\r\n$ "
            received = b""
            while len(received) < len(greeting):
                chunk = connection.recv(4096)
                assert chunk
                received += chunk
            assert received == greeting
            vm.serial.wait_for(b"devmgr\x1b[0m ready", timeout=60)
            assert_idle(vm)
    else:
        vm.start()
        vm.serial.wait_for(b"devmgr\x1b[0m ready", timeout=60)
        assert_idle(vm)
        vm.kill()
    output = vm.serial.path.read_text()
    assert "_PASS" not in output
    assert "PROBE" not in output
    assert "devmgr-observer" not in output
    assert "DEVMGR_ALLOCATOR" not in output
    assert "DEVMGR_FATAL" not in output


@pytest.mark.parametrize("cpus", [1, 2], ids=["up", "smp"])
@pytest.mark.parametrize("release", [False, True], ids=["debug", "release"])
@pytest.mark.parametrize("fault", [False, True], ids=["discovery", "fatal"])
def test_devmgr_discovery(system, release, fault, cpus):
    modules = ("platform/system/module.nix", "tests/boot/devmgr_checks.nix")
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
        assert (
            "DEVMGR_FATAL reboot=true reason=critical service stopped or faulted"
            in output
        )
        return
    vm.serial.wait_for(
        b"DEVMGR_BANK_PASS retained=true shared=true coalesced=true mmio_charge=0",
        timeout=60,
    )
    vm.serial.wait_for(b"DEVMGR_ALLOCATOR allocator=mimalloc")
    vm.serial.wait_for(b"PCI discovery complete:", timeout=60)
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
        modules=(
            "platform/system/module.nix",
            "tests/boot/devmgr_checks.nix",
            "tests/boot/devmgr_stall.nix",
        ),
        cpus=1,
    )
    vm.start()
    assert vm.process.wait(timeout=60) == 0
    output = vm.serial.path.read_text()
    assert "DEVMGR_FATAL reboot=true reason=startup timeout" in output
    assert "devmgr\x1b[0m ready" not in output
