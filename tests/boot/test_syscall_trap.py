import pytest


@pytest.mark.parametrize(
    ("cpus", "extra"),
    [
        (1, ("tests/boot/syscall_trap_up.nix",)),
        (2, ()),
        (2, ("tests/boot/rust_release.nix",)),
        (2, ("tests/boot/syscall_trap_no_skim.nix",)),
    ],
    ids=["up", "smp", "release-smp", "no-skim-smp"],
)
def test_foreign_syscalls_trap(system, cpus, extra):
    vm = system(modules=("tests/boot/syscall_trap.nix",) + extra, cpus=cpus)
    vm.start()
    vm.serial.wait_for(
        b"SYSCALL_TRAP_PASS raw_rsp=true arguments=true selectors=true int_gp=true native=true",
        timeout=30,
    )
    assert vm.monitor.execute("query-status")["running"]
    vm.kill()
