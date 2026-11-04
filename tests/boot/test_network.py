"""One end-to-end event-loop/pool smoke, not a profile/failure matrix."""

import os
import socket
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack

import pytest

REQUEST = b"GET / HTTP/1.1\r\nHost: horn\r\n\r\n"


def response(connection):
    data = bytearray()
    while chunk := connection.recv(4096):
        data.extend(chunk)
    assert data.startswith(b"HTTP/1.1 200 OK\r\n"), data
    assert data.endswith(b"Hello from Horn!\n"), data


def exercise_http(port):
    def connect():
        return socket.create_connection(("127.0.0.1", port), timeout=10)

    def request(_):
        with connect() as connection:
            connection.sendall(REQUEST)
            response(connection)

    with ExitStack() as stack:
        # More than the old eight slots. None of these requests can complete yet.
        slow = [stack.enter_context(connect()) for _ in range(12)]
        for connection in slow:
            connection.sendall(b"GET / HTTP/1.1\r\nHost: ")
        # A sequential server blocks behind the unfinished first request.
        with ThreadPoolExecutor(max_workers=8) as workers:
            list(workers.map(request, range(8)))
        for connection in slow:
            connection.sendall(b"horn\r\n\r\n")
        for connection in slow:
            response(connection)
    # Exercise retirement/reuse beyond the live capability quota, while the
    # earlier TCP slots can still be in FIN/TIME_WAIT and consume buffer budget.
    for i in range(40):
        request(i)


@pytest.mark.parametrize("cpus", [1, 2], ids=["up", "smp"])
@pytest.mark.parametrize(
    "accel",
    [
        "tcg",
        pytest.param(
            "kvm",
            marks=pytest.mark.skipif(
                not os.access("/dev/kvm", os.R_OK | os.W_OK), reason="KVM unavailable"
            ),
        ),
    ],
)
def test_http_event_loop_and_growing_pool(system, cpus, accel):
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    vm = system(
        modules=("platform/system/console.nix",),
        cpus=cpus,
        accel=accel,
        extra_args=(("-cpu", "host,migratable=off") if accel == "kvm" else ())
        + (
            "-device",
            "intel-iommu,intremap=on,caching-mode=on,device-iotlb=off,aw-bits=48",
            "-netdev",
            f"user,id=net0,restrict=on,hostfwd=tcp:127.0.0.1:{port}-:80",
            "-device",
            "virtio-net-pci,netdev=net0,addr=05.0,disable-legacy=on,iommu_platform=on",
            "-chardev",
            "null,id=console",
            "-device",
            "virtio-serial-pci,id=vserial,addr=04.0,disable-legacy=on,iommu_platform=on",
            "-device",
            "virtconsole,chardev=console,bus=vserial.0",
        ),
    )
    vm.start()
    vm.serial.wait_for("listening on TCP port 80", timeout=30)
    exercise_http(port)
    log = vm.serial.path.read_text()
    assert "panicked" not in log and "DMA fault from device" not in log, log
    assert "shared clock page mapped (read-only)" in log, log
    assert (
        "hardware clock (via IPC)" in log or "local TSC enabled (fast path)" in log
    ), log
