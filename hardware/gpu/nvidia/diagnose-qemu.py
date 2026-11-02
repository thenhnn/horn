#!/usr/bin/env python3
"""Bounded read-only QEMU observer, run by run-vfio.sh with its existing UID.

No target process attachment/signals, guest stop/continue, PCI access or reset. QMP
capabilities only negotiates this connection; subsequent commands are queries.
/proc/*/stack is a kernel stack, not a userspace backtrace.
"""
import json
import os
from pathlib import Path
import socket
import signal
import sys
import time

run = Path(sys.argv[1])
marker = os.fsencode(str(run / "qmp.sock"))
start = time.monotonic()


def log(*args):
    print(f"elapsed={time.monotonic() - start:.3f}", *args, flush=True)


def find_qemu():
    for proc in Path("/proc").iterdir():
        if not proc.name.isdecimal():
            continue
        try:
            cmd = (proc / "cmdline").read_bytes().split(b"\0")
            if cmd and b"qemu-system" in cmd[0] and any(marker in arg for arg in cmd):
                return proc
        except OSError:
            pass
    return None


def read(path):
    try:
        return path.read_text().strip()
    except OSError as error:
        return f"unavailable: {error}"


def qmp():
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.settimeout(1)
            sock.connect(str(run / "qmp.sock"))
            log("QMP connected")
            with sock.makefile("rb") as stream:
                def message():
                    remaining = 50 - (time.monotonic() - start)
                    if remaining <= 0:
                        raise TimeoutError("observer budget exhausted")
                    sock.settimeout(min(1, remaining))
                    line = stream.readline(1024 * 1024)
                    if not line:
                        raise EOFError("QMP EOF")
                    return json.loads(line)

                greeting = message()
                log("QMP greeting", json.dumps(greeting))
                if "QMP" not in greeting:
                    return
                for index, (command, args) in enumerate([
                    ("qmp_capabilities", {}),
                    ("query-status", {}),
                    ("query-cpus-fast", {}),
                    ("human-monitor-command", {"command-line": "info registers"}),
                ]):
                    sock.sendall((json.dumps({"execute": command, "arguments": args, "id": index}) + "\n").encode())
                    # Finite event budget as well as per-read timeout.
                    for _ in range(32):
                        reply = message()
                        log("QMP", command, json.dumps(reply))
                        if reply.get("id") == index:
                            break
                    else:
                        log("QMP event budget exhausted")
                        return
    except (OSError, EOFError, ValueError) as error:
        log("QMP unavailable", repr(error))


def observer_timeout(_signal, _frame):
    raise TimeoutError("observer's own 50 s budget expired")


signal.signal(signal.SIGALRM, observer_timeout)
signal.alarm(50)  # Only this observer, never QEMU or the launcher.
try:
    # Starts after build, immediately before the launch; never launches itself.
    proc = None
    while time.monotonic() - start < 10:
        proc = find_qemu()
        if proc is not None:
            break
        time.sleep(0.1)
    if proc is None:
        log("QEMU not found within observer startup budget")
        sys.exit(0)
    log("QEMU pid", proc.name)
    for target in (3, 15, 35):
        if time.monotonic() - start >= 50:
            break
        delay = target - (time.monotonic() - start)
        if delay > 0:
            time.sleep(delay)
        if not (proc / "cmdline").exists():
            log("QEMU exited")
            break
        # Check identity again; do not inspect a recycled PID.
        if marker not in (proc / "cmdline").read_bytes():
            log("QEMU identity no longer matches")
            break
        log("SNAPSHOT", target)
        log("process", " | ".join(line for line in read(proc / "status").splitlines()
                                 if line.startswith(("Name:", "State:", "Threads:", "VmRSS:"))))
        for task in sorted((proc / "task").iterdir(), key=lambda path: int(path.name)):
            log("THREAD", task.name, read(task / "comm"), "wchan=", read(task / "wchan"))
            log("syscall", task.name, read(task / "syscall"))
            log("kernel-stack", task.name, read(task / "stack"))
        qmp()
    log("observer finished")
except OSError as error:
    log("observer ended", repr(error))
