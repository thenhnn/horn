def test_idl_child_process_demo(system):
    vm = system(modules=("examples/idl/demo/module.nix",))
    vm.start()
    vm.serial.wait_for(b"IDL_DEMO_PASS value=41")
    vm.serial.wait_for(b"MEMORY_RPC_PASS zeroed=true rollback=true reclaimed=true", timeout=15)
    assert vm.monitor.execute("query-status")["running"]
    vm.kill()
