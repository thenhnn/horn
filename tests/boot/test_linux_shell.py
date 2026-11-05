"""Real Linux TTY input, processes and job control over the capability console."""

import os
import time

import pytest
from test_virtio_console import assert_idle, console_connection


@pytest.mark.skipif(not os.access("/dev/kvm", os.R_OK | os.W_OK), reason="requires KVM")
@pytest.mark.parametrize("profile", ["pc-dev", "pc-release"])
def test_linux_interactive_shell(system, profile):
    vm = system(
        profile=profile, modules=("platform/linux/shell.nix",), cpus=2, accel="kvm"
    )
    transcript = bytearray()
    with console_connection(vm) as connection:

        def prompt(timeout=30, marker=None):
            connection.settimeout(timeout)
            output = bytearray()
            try:
                while not output.endswith(b"linux# ") or (
                    marker and marker not in output
                ):
                    chunk = connection.recv(4096)
                    assert chunk, "console closed"
                    output.extend(chunk)
                    transcript.extend(chunk)
            finally:
                (vm.directory / "console.log").write_bytes(transcript)
            return bytes(output)

        def command(text):
            connection.sendall(text.encode() + b"\n")
            return prompt()

        boot = prompt(60)
        assert b"Horn Linux interactive shell" in boot
        assert b"job control turned off" not in boot
        assert b"SHELL_EXEC_OK" in command("printf 'SHELL_%s\\n' EXEC_OK")
        assert b"Hello from the Horn initramfs!" in command("cat /demo.txt")
        assert b"PIPELINE_OK" in command("printf 'pipeline_ok\\n' | tr a-z A-Z")
        assert b"FILES_OK" in command(
            "echo files > /tmp/check; cat /tmp/check | tr a-z A-Z | sed 's/$/_OK/'"
        )
        assert b"init\r\n" in command("cat /proc/1/comm")
        assert b"/dev/hvc0" in command("tty")
        assert b"TTY_OK" in command("test -t 0 && test -t 1 && printf 'TTY_%s\\n' OK")
        assert b"40 100" in command("stty rows 40 cols 100; stty size")
        # Repeated reads/writes exercise one-shot rearming rather than a prompt
        # printed once during boot. Output is generated, not merely echoed input.
        for _ in range(12):
            assert b"WAKE_OK" in command("printf 'WAKE_%s\\n' OK")
        # Withhold reads until TX backs up, then inject input while Linux is
        # blocked writing. Its single console watch must cover RX and TX.
        connection.sendall(
            b"awk 'BEGIN { for(i=0;i<10000;i++) print \"0123456789012345678901234567890123456789012345678901234567890123456789\" }'; printf 'BULK_%s\\n' DONE\n"
        )
        time.sleep(0.3)
        connection.sendall(b"printf 'QUEUED_%s\\n' OK\n")
        bulk = prompt(60, marker=b"QUEUED_OK")
        assert b"BULK_DONE" in bulk
        assert (
            bulk.count(
                b"0123456789012345678901234567890123456789012345678901234567890123456789"
            )
            >= 10000
        )
        assert_idle(vm)
        assert b"RESUME_OK" in command("printf 'RESUME_%s\\n' OK")
        connection.sendall(b"sleep 30\n")
        time.sleep(0.3)
        connection.sendall(b"\x03")
        prompt(5)
        assert b"INT_STATUS=130" in command("printf 'INT_STATUS=%s\\n' \"$?\"")
        # init must reap and respawn the shell, not die with PID 1.
        assert b"Horn Linux interactive shell" in command("exit")
        assert b"RESPAWN_OK" in command("printf 'RESPAWN_%s\\n' OK")
        serial = vm.serial.path.read_text(errors="replace")
        for failure in ("Kernel panic", "panicked at", " fault: ", " timed out"):
            assert failure not in serial + transcript.decode(errors="replace")
        assert vm.monitor.execute("query-status")["running"]
