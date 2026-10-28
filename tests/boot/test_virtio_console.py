import re
import socket
from contextlib import contextmanager

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


def read_banner(connection, vm):
    received = b""
    while b"\n" not in received:
        part = connection.recv(4096)
        assert part, vm.serial.path.read_text()
        received += part
    assert received == b"HORN_VIRTIO_CONSOLE_READY\n"
    return received


@pytest.mark.parametrize("cpus", [1, 2], ids=["up", "smp"])
@pytest.mark.parametrize("release", [False, True], ids=["debug", "release"])
def test_virtio_console_dma_irq(system, release, cpus):
    modules = ("examples/devmgr/module.nix", "tests/boot/virtio_console.nix")
    if release:
        modules += ("tests/boot/rust_release.nix",)
    vm = system(modules=modules, cpus=cpus)
    with console_connection(vm) as connection:
        received = read_banner(connection, vm)
        # Thousands of map/unmap/close operations exercise pin-token and CSlot
        # reuse; a short banner alone would miss historical metadata exhaustion.
        payload = b"horn capability DMA echo\n" * 48
        connection.sendall(payload)
        echoed = b""
        while len(echoed) < len(payload):
            part = connection.recv(4096)
            assert part, vm.serial.path.read_text()
            echoed += part
        assert echoed == payload
        (vm.directory / "console.log").write_bytes(received + echoed)
        vm.serial.wait_for(
            b"VTD_ACTIVE translation=true interrupt_remap=true compatibility=false"
        )
        vm.serial.wait_for(
            b"VIRTIO_CONSOLE_READY allocator=mimalloc transport=upstream dma=iova irq=msix"
        )
        vm.serial.wait_for(b"VIRTIO_CONSOLE_IRQ delivered=true")
        assert "VTD_FAULT" not in vm.serial.path.read_text()
        assert vm.monitor.execute("query-status")["running"]
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
            read_banner(connection, vm)
        vm.serial.wait_for(b"DEVICE_CAP_CHECK_PASS wrong_vm_object=denied")
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
        if probe != "stale":
            assert "VIRTIO_CONSOLE_READY" not in output
        assert "DEVMGR_FATAL" not in output
        assert vm.monitor.execute("query-status")["running"]
        vm.kill()
