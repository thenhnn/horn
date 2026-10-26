def test_idl_child_process_demo(system):
    vm = system(modules=("examples/idl/demo/module.nix",))
    vm.start()
    vm.serial.wait_for(b"IDL_DEMO_PASS value=41")
    assert vm.monitor.execute("query-status")["running"]
    vm.kill()
