import json
import re
import subprocess
import tempfile
from pathlib import Path

import pytest

from .vm import VM

ROOT = Path(__file__).resolve().parents[2]


@pytest.fixture
def system(request):
    machines = []

    def build(
        *, profile="pc-dev", modules=(), cpus=1, memory=512, devices=(), extra_args=()
    ):
        artifacts = ROOT / "work" / "pytest"
        artifacts.mkdir(parents=True, exist_ok=True)
        name = re.sub(r"[^a-zA-Z0-9_.-]", "_", request.node.name)
        directory = Path(tempfile.mkdtemp(prefix=f"{name}-", dir=artifacts))
        module_arg = "[ " + " ".join(f"./{path}" for path in modules) + " ]"
        command = [
            "nix-build",
            "nix",
            "-A",
            "sel4.out",
            "-A",
            "root-task.package",
            "--argstr",
            "profile",
            profile,
            "--arg",
            "modules",
            module_arg,
            "--out-link",
            str(directory / "system"),
        ]
        (directory / "build-command.json").write_text(
            json.dumps(command, indent=2) + "\n"
        )
        with (directory / "build.log").open("w") as log:
            result = subprocess.run(
                command,
                cwd=ROOT,
                stdout=subprocess.PIPE,
                stderr=log,
                text=True,
                timeout=600,
                check=False,
            )
        assert result.returncode == 0, f"System build failed: {directory / 'build.log'}"
        kernel, root_task = map(Path, result.stdout.splitlines())
        machine = VM(
            directory,
            kernel,
            root_task,
            cpus=cpus,
            memory=memory,
            devices=devices,
            extra_args=extra_args,
        )
        machines.append(machine)
        return machine

    yield build
    for machine in machines:
        machine.kill()
