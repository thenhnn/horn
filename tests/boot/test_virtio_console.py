import os
import re
import socket
import time
from contextlib import contextmanager
from pathlib import Path

import pytest


@contextmanager
def console_connection(vm):
    path = vm.directory / "console.sock"
    with socket.socket(socket.AF_UNIX) as listener:
        listener.bind(str(path))
        listener.listen(1)
        listener.settimeout(60)
        vm.extra_args.extend(
            (
                "-device",
                "intel-iommu,intremap=on,caching-mode=on,device-iotlb=off,aw-bits=48",
                "-chardev",
                f"socket,id=console,path={path}",
                "-device",
                "virtio-serial-pci,id=vserial,addr=04.0,disable-legacy=on,iommu_platform=on",
                "-device",
                "virtconsole,chardev=console,bus=vserial.0",
            )
        )
        vm.start()
        connection, _ = listener.accept()
        with connection:
            connection.settimeout(60)
            yield connection


def read_prompt(connection, vm, count=1):
    received = b""
    while received.count(b"$ ") < count or not received.endswith(b"$ "):
        part = connection.recv(4096)
        assert part, vm.serial.path.read_text()
        received += part
    return received


def assert_idle(vm):
    # Linux reports process-wide user+system ticks, including QEMU's vCPU
    # threads. Measure host CPU, not guest wall time or scheduler Yield calls.
    def cpu_seconds():
        stat = (
            Path(f"/proc/{vm.process.pid}/stat").read_text().rsplit(")", 1)[1].split()
        )
        return (int(stat[11]) + int(stat[12])) / os.sysconf("SC_CLK_TCK")

    time.sleep(0.2)
    before = cpu_seconds()
    started = time.monotonic()
    time.sleep(2)
    elapsed = time.monotonic() - started
    fraction = (cpu_seconds() - before) / elapsed
    (vm.directory / "idle-cpu.txt").write_text(
        f"cpu_seconds_per_wall_second={fraction:.4f}\n"
    )
    assert fraction < 0.35, f"Idle QEMU burns {fraction:.1%} of one host CPU"


@pytest.mark.parametrize("cpus", [1, 2], ids=["up", "smp"])
@pytest.mark.parametrize("release", [False, True], ids=["debug", "release"])
def test_virtio_console_dma_irq(system, release, cpus):
    modules = ("examples/devmgr/module.nix", "tests/boot/virtio_console.nix")
    if release:
        modules += ("tests/boot/rust_release.nix",)
    vm = system(modules=modules, cpus=cpus)
    with console_connection(vm) as connection:
        received = read_prompt(connection, vm)
        assert received == b"\x1b[1;36mHorn v0.1 started\x1b[0m\r\n$ "
        connection.sendall(b"pci\n")
        devices = read_prompt(connection, vm)
        assert b"0000:00:04.0 1af4:" in devices
        connection.sendall(b"ps\n")
        processes = read_prompt(connection, vm)
        assert b"devmgr" in processes
        assert b"virtio-console" in processes
        assert b"horn-shell" in processes
        assert b"Active" in processes
        # Repeated real commands exercise IPC, partial writes and notification
        # rearming, without an alternate echo implementation in the image.
        connection.sendall(b"help\n" * 48)
        commands = read_prompt(connection, vm, count=48)
        assert commands.count(b"list PCIe devices") == 48
        (vm.directory / "console.log").write_bytes(
            received + devices + processes + commands
        )
        vm.serial.wait_for(b"DEVMGR_ALLOCATOR allocator=mimalloc")
        vm.serial.wait_for(
            b"VTD_ACTIVE translation=true interrupt_remap=true compatibility=false"
        )
        vm.serial.wait_for(b"VTD_FAULT_IRQ armed=true")
        vm.serial.wait_for(
            b"VIRTIO_CONSOLE_READY allocator=mimalloc transport=upstream dma=iova irq=msix"
        )
        vm.serial.wait_for(b"VIRTIO_CONSOLE_IRQ delivered=true")
        assert "VTD_FAULT source=" not in vm.serial.path.read_text()
        assert vm.monitor.execute("query-status")["running"]
        assert_idle(vm)
        # Ensure a genuinely sleeping system still wakes and handles new input.
        connection.sendall(b"help\n")
        resumed = read_prompt(connection, vm)
        assert b"list supervised processes" in resumed
        vm.kill()


@pytest.mark.parametrize("probe", ["outside", "stale"])
def test_virtio_console_denied_dma(system, probe):
    vm = system(
        modules=(
            "examples/devmgr/module.nix",
            "tests/boot/virtio_console.nix",
            f"tests/boot/virtio_console_{probe}.nix",
        )
    )
    with console_connection(vm) as connection:
        if probe == "stale":
            read_prompt(connection, vm)
            connection.sendall(b"help\n")
        # This later marker follows the wrong-cap assertions too. The early
        # diagnostic can interleave bytewise with timerd startup on debug UART.
        vm.serial.wait_for(b"DEVICE_MMIO_LEASE_PASS quota=8 cycles=16 scoped=true")
        vm.serial.wait_for(f"DMA_PROBE {probe}=true".encode())
        vm.serial.wait_for(b"VTD_FAULT source=0020", timeout=60)
        vm.serial.wait_for(b"read=true overflow=false", timeout=60)
        output = vm.serial.path.read_text()
        fault = re.search(
            r"VTD_FAULT source=0020 address=([0-9a-f]+) reason=([0-9a-f]+) read=true overflow=false",
            output,
        )
        assert fault, output
        addresses = re.findall(
            rf"DMA_PROBE {probe}=true (?:drained=true warmed=true )?address=([0-9a-f]+)",
            output,
        )
        assert fault.group(1) in addresses, output
        assert int(fault.group(2), 16) != 0
        assert "DEVMGR_FATAL" not in output
        assert vm.monitor.execute("query-status")["running"]
        assert_idle(vm)
        vm.kill()
