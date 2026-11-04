import pytest
from test_virtio_console import assert_idle, console_connection


@pytest.mark.parametrize("scenario", ["no_ready", "early_fault"])
def test_supervisor_requires_explicit_ready(system, scenario):
    vm = system(
        modules=(
            "platform/system/module.nix",
            "tests/boot/devmgr_checks.nix",
            f"tests/boot/supervisor_{scenario}.nix",
        ),
        cpus=1,
        extra_args=("-device", "virtio-serial-pci", "-device", "virtio-net-pci"),
    )
    vm.start()
    assert vm.process.wait(timeout=60) == 0
    output = vm.serial.path.read_text()
    assert "devmgr\x1b[0m ready" not in output
    if scenario == "no_ready":
        assert "DEVMGR_READY_WITHHELD test=true" in output
        assert "DEVMGR_RPC_PASS" in output
        assert "DEVMGR_FATAL reboot=true reason=startup timeout" in output
    else:
        assert "DEVMGR_ALLOCATOR allocator=mimalloc" in output
        assert (
            "DEVMGR_FATAL reboot=true reason=critical service stopped or faulted"
            in output
        )


@pytest.mark.parametrize("mode", ["orderly", "force", "timeout", "fault", "exit"])
@pytest.mark.parametrize(
    "release,cpus", [(False, 1), (True, 2)], ids=["debug-up", "release-smp"]
)
def test_supervisor_driver_revoke(system, mode, release, cpus):
    modules = (
        "platform/system/module.nix",
        "tests/boot/devmgr_checks.nix",
        f"tests/boot/supervisor_{mode}.nix",
    )
    if release:
        modules += ("tests/boot/rust_release.nix",)
    vm = system(modules=modules, cpus=cpus)
    with console_connection(vm):
        vm.serial.wait_for(
            b"released; RAM returned, owner acknowledged",
            timeout=60,
        )
        vm.serial.wait_for(
            b"DEVMGR_DRIVER_REVOKE dma_drained=true irq_closed=true mmio_revoked=true"
        )
        vm.serial.wait_for(b"DEVMGR_ASSIGNMENT_PASS retired=true")
        output = vm.serial.path.read_text()
        assert "starting \x1b[1mvirtio-console@" in output
        if mode == "orderly":
            assert "Console stopped" in output
            assert "DEVMGR_DRIVER_REVOKE cpu_stopped=true orderly=true" in output
        else:
            assert "DEVMGR_DRIVER_REVOKE cpu_stopped=true orderly=false" in output
        if mode in ("orderly", "force", "timeout"):
            assert "requested stop" in output
            assert "DEVMGR_ASSIGNMENT_PASS retired=true" in output
        assert (
            "DEVMGR_DRIVER_REVOKE dma_drained=true irq_closed=true mmio_revoked=true"
            in output
        )
        assert "DEVMGR_FATAL" not in output
        assert "DMA fault from device" not in output
        assert vm.monitor.execute("query-status")["running"]
        assert_idle(vm)
