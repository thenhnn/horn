"""One end-to-end event-loop/pool smoke, not a profile/failure matrix."""

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
def test_http_event_loop_and_growing_pool(system, cpus):
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    vm = system(
        modules=("examples/devmgr/console.nix",),
        cpus=cpus,
        extra_args=(
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
    vm.serial.wait_for("HTTP_DEMO_READY", timeout=30)
    exercise_http(port)
    log = vm.serial.path.read_text()
    assert "panicked" not in log and "VTD_FAULT source=" not in log, log
    if cpus > 1:
        assert "fast_tsc=false" in log, log
