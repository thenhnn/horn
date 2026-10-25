def test_sel4_boots(system):
    vm = system(modules=("tests/boot/sel4_boots.nix",))
    vm.start()
    vm.serial.wait_for(b"hi im root task")
    assert vm.monitor.execute("query-status")["running"]
    vm.kill()
