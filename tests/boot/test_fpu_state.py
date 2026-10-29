"""Small targeted kernel API smoke, not a profile matrix."""

import os

import pytest


@pytest.mark.skipif(not os.access("/dev/kvm", os.R_OK | os.W_OK), reason="requires KVM")
def test_extended_fp_state(system):
    vm = system(modules=("tests/boot/fpu_state.nix",), cpus=2, accel="kvm")
    vm.start()
    vm.serial.wait_for("FPU_STATE_PASS avx=true", timeout=15)
    output = vm.serial.path.read_text()
    assert "FPU_STATE_FAIL" not in output, output
    assert vm.monitor.execute("query-status")["running"]
