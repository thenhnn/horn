"""One ARCH=horn Linux CPU: initramfs, trapped syscalls, clock and sleep."""

import os
import time

import pytest


@pytest.mark.skipif(not os.access("/dev/kvm", os.R_OK | os.W_OK), reason="requires KVM")
def test_linux_initramfs_pid1_and_clock(system):
    vm = system(modules=("platform/linux/module.nix",), cpus=2, accel="kvm")
    console = vm.directory / "console.log"
    vm.extra_args.extend(
        (
            "-device",
            "intel-iommu,intremap=on,caching-mode=on,device-iotlb=off,aw-bits=48",
            "-chardev",
            f"file,id=console,path={console}",
            "-device",
            "virtio-serial-pci,id=vserial,addr=04.0,disable-legacy=on,iommu_platform=on",
            "-device",
            "virtconsole,chardev=console,bus=vserial.0",
        )
    )
    vm.start()
    deadline = time.monotonic() + 15
    ready_at = None
    while True:
        serial = vm.serial.path.read_text(errors="replace")
        output = console.read_text(errors="replace") if console.exists() else ""
        diagnostics = serial + "\n" + output
        assert vm.process.poll() is None, diagnostics
        for failure in (
            " fault: ",
            " timed out",
            "Kernel panic",
            "panicked at",
            "/init: startup failed",
            "EXECUTION_MAP_FAIL",
        ):
            assert failure not in diagnostics, diagnostics
        if "/init: monotonic clock and sleep work" in output:
            ready_at = ready_at or time.monotonic()
            if time.monotonic() - ready_at >= 2:
                break
        assert time.monotonic() < deadline, diagnostics
        time.sleep(0.05)
    assert "/init: FP/SIMD state survives syscalls and signals" in output, diagnostics
    assert "Linux /init running as PID 1" in output, diagnostics
    assert "linux ready" in serial, diagnostics
    assert vm.monitor.execute("query-status")["running"]
