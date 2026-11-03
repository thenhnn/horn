"""Compile-only in this change; execution remains an explicit test action."""

import importlib.util
from pathlib import Path

import pytest

spec = importlib.util.spec_from_file_location(
    "service_compiler",
    Path(__file__).resolve().parents[2] / "tools/services/compile.py",
)
compiler = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compiler)


def definitions(tmp_path):
    def service():
        return {
            "elf": "/service.elf",
            "ram": 8 * 1024 * 1024,
            "activation": "automatic",
            "requires": {},
            "namespaces": {},
            "grants": {},
            "objects": {},
            "exports": [],
            "after": [],
            "publish": [],
        }

    boot = service()
    boot.update(activation="boot")
    timer = service()
    timer["requires"] = {"source": {"path": "/devices/clock", "select": "first"}}
    timer["objects"] = {
        "server": {
            "kind": "Endpoint",
            "bindings": {"service": {"badge": None, "rights": 15}},
            "bindNotification": False,
        },
        "events": {
            "kind": "Notification",
            "bindings": {
                "events": {"badge": None, "rights": 15},
                "signal": {"badge": 1, "rights": 1},
            },
            "bindNotification": True,
        },
    }
    return {"boot": boot, "timer": timer}


def test_logger_dependency_is_injected_without_mutating_input(tmp_path):
    services = definitions(tmp_path)
    logger = {**services["timer"], "requires": {}}
    logger["exports"] = [
        {
            "path": "/services/log",
            "object": "server",
            "badge": 16,
            "rights": 14,
            "supervisorOnly": True,
        }
    ]
    services["logd"] = logger
    services["timer"]["logging"] = "/services/log"
    result = compiler.compile_services(services)
    assert 'ContextBinding{name:"horn.log",slot:6}' in result
    assert 'path:"/services/log",first:false,slot:6' in result
    assert "_log" not in services["timer"]["requires"]


def test_logger_cannot_depend_on_itself(tmp_path):
    services = definitions(tmp_path)
    services["timer"]["logging"] = "/services/log"
    services["timer"]["exports"] = [
        {
            "path": "/services/log",
            "object": "server",
            "badge": 16,
            "rights": 14,
            "supervisorOnly": True,
        }
    ]
    with pytest.raises(AssertionError, match="dependency cycle"):
        compiler.compile_services(services)


def test_abi2_boot_has_named_context(tmp_path):
    services = definitions(tmp_path)
    result = compiler.compile_services(services)
    assert 'name:"boot"' in result
    assert 'ContextBinding{name:"horn.vm",slot:1}' in result
    assert 'context:&[],name:"boot"' not in result


def test_boot_cannot_silently_ignore_runtime_dependency(tmp_path):
    services = definitions(tmp_path)
    services["boot"]["requires"] = {
        "later": {"path": "/devices/clock", "select": "first"}
    }
    with pytest.raises(AssertionError, match="boot cannot wait"):
        compiler.compile_services(services)


def test_abi2_assigns_private_slots_and_sorted_runtime_names(tmp_path):
    result = compiler.compile_services(definitions(tmp_path))
    assert 'context:&[],name:"boot"' not in result
    assert 'ContextBinding{name:"horn.address-space",slot:2}' in result
    assert 'ContextBinding{name:"horn.vm",slot:1}' in result
    assert 'ContextBinding{name:"source",slot:9}' in result
    timer = next(line for line in result.splitlines() if 'name:"timer",' in line)
    assert timer.index('name:"events"') < timer.index('name:"horn.address-space"')
    assert timer.index('name:"horn.vm"') < timer.index('name:"source"')


@pytest.mark.parametrize("name", ["horn.hidden", "vm", "bad name", "é"])
def test_reserved_and_invalid_context_names_rejected(tmp_path, name):
    services = definitions(tmp_path)
    services["timer"]["requires"][name] = services["timer"]["requires"].pop("source")
    with pytest.raises(AssertionError):
        compiler.compile_services(services)


@pytest.mark.parametrize(
    "obsolete",
    [{"abi": 1}, {"abi": 2}, {"bootstrap": "/caps.json"}, {"capset": "TimerCaps"}],
)
def test_obsolete_startup_options_are_rejected(tmp_path, obsolete):
    services = definitions(tmp_path)
    services["timer"].update(obsolete)
    with pytest.raises(AssertionError, match="obsolete startup options"):
        compiler.compile_services(services)


def test_abi2_rejects_duplicate_ports_and_legacy_capsets(tmp_path):
    services = definitions(tmp_path)
    services["timer"]["objects"]["server"]["bindings"]["source"] = {
        "badge": None,
        "rights": 15,
    }
    with pytest.raises(AssertionError):
        compiler.compile_services(services)
    services = definitions(tmp_path)
    services["timer"]["capset"] = "TimerCaps"
    with pytest.raises(AssertionError):
        compiler.compile_services(services)
