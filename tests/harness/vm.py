import json
import socket
import subprocess
import time
from pathlib import Path


class Serial:
    def __init__(self, process, path):
        self.process = process
        self.path = path
        self.position = 0

    def write(self, data):
        if isinstance(data, str):
            data = data.encode()
        self.process.stdin.write(data)
        self.process.stdin.flush()

    def wait_for(self, expected, timeout=60):
        if isinstance(expected, str):
            expected = expected.encode()
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            with self.path.open("rb") as log:
                log.seek(self.position)
                output = log.read()
            if self.process.poll() is not None:
                raise RuntimeError(
                    f"QEMU exited with {self.process.returncode}: {self.path}\n"
                    + output.decode(errors="replace")
                )
            found = output.find(expected)
            if found >= 0:
                end = found + len(expected)
                self.position += end
                return output[:end]
            time.sleep(0.05)
        raise TimeoutError(
            f"Waiting for {expected!r}: {self.path}\n"
            + self.path.read_text(errors="replace")
        )


class Monitor:
    def __init__(self, path, process, timeout=10):
        self.socket = socket.socket(socket.AF_UNIX)
        self.events = []
        self.sequence = 0
        try:
            deadline = time.monotonic() + timeout
            while True:
                try:
                    self.socket.connect(str(path))
                    break
                except (FileNotFoundError, ConnectionRefusedError):
                    if process.poll() is not None:
                        raise RuntimeError(f"QEMU exited before QMP connected: {path}")
                    if time.monotonic() >= deadline:
                        raise TimeoutError(f"Waiting for QMP: {path}") from None
                    time.sleep(0.05)
            self.socket.settimeout(timeout)
            self.stream = self.socket.makefile("rb")
            greeting = self._read()
            if "QMP" not in greeting:
                raise RuntimeError(f"Invalid QMP greeting: {greeting}")
            self.execute("qmp_capabilities")
        except BaseException:
            self.close()
            raise

    def _read(self):
        line = self.stream.readline()
        if not line:
            raise RuntimeError("QMP disconnected")
        return json.loads(line)

    def execute(self, command, arguments=None):
        self.sequence += 1
        request = {"execute": command, "id": self.sequence}
        if arguments is not None:
            request["arguments"] = arguments
        self.socket.sendall(json.dumps(request).encode() + b"\n")
        while True:
            response = self._read()
            if "event" in response:
                self.events.append(response)
                continue
            if response.get("id") != self.sequence:
                raise RuntimeError(f"Unexpected QMP response: {response}")
            if "error" in response:
                raise RuntimeError(f"QMP {command}: {response['error']}")
            return response["return"]

    def close(self):
        if hasattr(self, "stream"):
            self.stream.close()
        self.socket.close()


class VM:
    def __init__(
        self,
        directory,
        kernel,
        root_task,
        *,
        cpus=1,
        memory=128,
        devices=(),
        extra_args=(),
    ):
        self.directory = Path(directory)
        self.kernel = Path(kernel)
        self.root_task = Path(root_task)
        self.cpus = cpus
        self.memory = memory
        self.devices = list(devices)
        self.extra_args = list(extra_args)
        self.monitor_socket = self.directory / "qmp.sock"
        self.process = None
        self.monitor = None
        self.serial = None
        self.log = None

    def start(self):
        if self.process is not None:
            raise RuntimeError("VM already started")
        for image in (
            self.kernel / "bin/kernel-multiboot.elf",
            self.root_task / "bin/task.elf",
        ):
            if not image.is_file():
                raise FileNotFoundError(image)
        command = [
            "qemu-system-x86_64",
            "-machine",
            "q35",
            "-accel",
            "tcg",
            "-cpu",
            "max",
            "-m",
            str(self.memory),
            "-smp",
            str(self.cpus),
            "-nodefaults",
            "-no-user-config",
            "-nic",
            "none",
            "-display",
            "none",
            "-monitor",
            "none",
            "-qmp",
            f"unix:{self.monitor_socket},server=on,wait=off",
            "-serial",
            "stdio",
            "-no-reboot",
            "-kernel",
            str(self.kernel / "bin/kernel-multiboot.elf"),
            "-initrd",
            str(self.root_task / "bin/task.elf"),
            "-append",
            "console_port=0x3f8 debug_port=0x3f8",
        ]
        for device in self.devices:
            command.extend(device.qemu_arguments())
        command.extend(self.extra_args)
        (self.directory / "command.json").write_text(
            json.dumps(command, indent=2) + "\n"
        )
        path = self.directory / "serial.log"
        self.log = path.open("wb")
        try:
            self.process = subprocess.Popen(
                command,
                stdin=subprocess.PIPE,
                stdout=self.log,
                stderr=subprocess.STDOUT,
            )
            self.serial = Serial(self.process, path)
            self.monitor = Monitor(self.monitor_socket, self.process)
        except BaseException:
            self.kill()
            raise

    def kill(self):
        try:
            if self.process is not None:
                if self.process.poll() is None:
                    self.process.kill()
                self.process.wait(timeout=5)
                self.process.stdin.close()
        finally:
            if self.monitor is not None:
                self.monitor.close()
            if self.log is not None:
                self.log.close()
