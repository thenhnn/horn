"""Compile Nix policy and IDL slot descriptions; no application/service kinds."""

import argparse
import json
from pathlib import Path


def quote(value):
    return json.dumps(value)


def badge(value):
    if value is None:
        return "None"
    number = int(value, 0) if isinstance(value, str) else value
    assert type(number) is int and 0 < number < 2**64
    return f"Some({number})"


def valid_path(value):
    assert isinstance(value, str) and value.startswith("/") and len(value) <= 96, value
    assert value == "/" or all(
        p
        and p not in (".", "..")
        and len(p) <= 64
        and all(32 < ord(c) < 127 and c not in "\\{}" for c in p)
        for p in value[1:].split("/")
    ), value


def compile_services(definitions):
    definitions = {
        name: {**service, "requires": dict(service["requires"])}
        for name, service in definitions.items()
    }
    for name, service in definitions.items():
        if service.get("logging") is not None:
            assert "_log" not in service["requires"], f"{name}: reserved logging port"
            valid_path(service["logging"])
            service["requires"]["_log"] = {
                "path": service["logging"],
                "select": "exact",
            }
    assert len(definitions) <= 8
    assert sum(s["activation"] == "boot" for s in definitions.values()) == 1
    exports = [("/services/procd", None)]
    for name, service in definitions.items():
        assert (
            name
            and len(name) <= 32
            and all(c.isascii() and (c.isalnum() or c in "-_") for c in name)
        ), name
        assert service["activation"] in ("boot", "automatic")
        for export in service["exports"]:
            valid_path(export["path"])
            assert export["object"] in service["objects"]
            assert service["objects"][export["object"]]["kind"] == "Endpoint"
            assert 0 < export["badge"] < 2**64 and 0 < export["rights"] <= 15
            exports.append((export["path"], name))
    assert len({p for p, _ in exports}) == len(exports), "duplicate exports"
    for name, service in definitions.items():
        if service.get("logging") is not None:
            assert any(
                export["path"] == service["logging"]
                and export.get("supervisorOnly", False)
                for provider in definitions.values()
                for export in provider["exports"]
            ), f"{name}: logging factory must be supervisor-only"
    graph = {name: set() for name in definitions}
    for name, service in definitions.items():
        for requirement in service["requires"].values():
            valid_path(requirement["path"])
            candidates = {
                provider
                for p, provider in exports
                if (
                    p == requirement["path"]
                    if requirement["select"] == "exact"
                    else p.startswith(requirement["path"].rstrip("/") + "/")
                )
            }
            # A first-provider requirement may be supplied dynamically by a device owner.
            assert candidates or requirement["select"] == "first", (
                f"{name}: missing provider"
            )
            graph[name].update(p for p in candidates if p is not None)
        for grant in service["namespaces"].values():
            valid_path(grant["path"])
        for dependency in service["after"]:
            valid_path(dependency)
            graph[name].update(
                provider
                for p, provider in exports
                if p == dependency and provider is not None
            )

    def visit(name, active, done):
        assert name not in active, f"dependency cycle through {name}"
        if name in done:
            return
        for dependency in graph[name]:
            visit(dependency, active | {name}, done)
        done.add(name)

    done = set()
    for name in graph:
        visit(name, set(), done)
    result = ["pub static SERVICES: &[ServiceDefinition] = &["]
    for name, service in sorted(definitions.items()):
        assert not ({"abi", "bootstrap", "capset"} & service.keys()), (
            f"{name}: obsolete startup options"
        )
        if service["activation"] == "boot":
            assert not service["requires"] and not service["after"], (
                f"{name}: boot cannot wait for runtime dependencies"
            )
            assert all(g["minReady"] == 0 for g in service["namespaces"].values()), (
                f"{name}: boot namespace cannot wait for READY"
            )
        ports = ["vm", "address_space", "lifecycle", "reply", "ready"]
        if service.get("processFactory", False):
            ports.append("process_factory")
        local = (
            list(service["requires"])
            + list(service["namespaces"])
            + list(service["grants"])
            + [p for obj in service["objects"].values() for p in obj["bindings"]]
        )
        assert len(local) == len(set(local)), f"{name}: duplicate context binding"
        for port in local:
            assert port and all(0x21 <= ord(c) <= 0x7E for c in port), port
            assert port not in ports and not port.startswith("horn."), port
        # Slot 0 is the process's own CNode. Physical placement stays private
        # to deployment/procd; no generated constants enter the service.
        slots = {port: i for i, port in enumerate(ports + sorted(local), 1)}
        assigned = set()

        def slot(port, slots=slots, assigned=assigned, name=name):
            assert port in slots, f"{name}: unknown bootstrap port {port}"
            assert port not in assigned, f"{name}: duplicate binding of {port}"
            assigned.add(port)
            return slots[port]

        base = {
            port: slot(port)
            for port in ("vm", "address_space", "lifecycle", "reply", "ready")
        }
        requirements = ",".join(
            f"Requirement{{path:{quote(r['path'])},first:{str(r['select'] == 'first').lower()},slot:{slot(local)}}}"
            for local, r in sorted(service["requires"].items())
        )
        namespaces = ",".join(
            f"NamespaceGrant{{path:{quote(r['path'])},slot:{slot(local)},min_ready:{r['minReady']}}}"
            for local, r in sorted(service["namespaces"].items())
        )
        objects = []
        for object_name, obj in sorted(service["objects"].items()):
            bindings = ",".join(
                f"ObjectSlot{{slot:{slot(port)},badge:{badge(binding['badge'])},rights:{binding['rights']}}}"
                for port, binding in sorted(obj["bindings"].items())
            )
            assert obj["kind"] in ("Endpoint", "Notification")
            assert not obj["bindNotification"] or obj["kind"] == "Notification"
            published = ",".join(
                f"Export{{supervisor_only:{str(r.get('supervisorOnly', False)).lower()},path:{quote(r['path'])},badge:{r['badge']},rights:{r['rights']}}}"
                for r in service["exports"]
                if r["object"] == object_name
            )
            objects.append(
                f"Object{{kind:ProcessKernelObject::{obj['kind']},slots:&[{bindings}],bind:{str(obj['bindNotification']).lower()},exports:&[{published}]}}"
            )
        grants = ",".join(
            f"BootGrant{{slot:{slot(port)},source:BootSource::{r['source']},rights:{r['rights']}}}"
            for port, r in sorted(service["grants"].items())
        )
        assert service["activation"] == "boot" or not grants
        after = ",".join(quote(p) for p in service["after"])
        published_prefixes = ",".join(quote(p) for p in service["publish"])
        for p in service["publish"]:
            valid_path(p)
        runtime_names = {
            "vm": "horn.vm",
            "address_space": "horn.address-space",
            "lifecycle": "horn.lifecycle",
            "reply": "horn.reply",
            "ready": "horn.ready",
            "process_factory": "horn.process-factory",
        }
        if service.get("logging") is not None:
            runtime_names["_log"] = "horn.log"
        context = (
            "&["
            + ",".join(
                f"ContextBinding{{name:{quote(runtime_names.get(p, p))},slot:{s}}}"
                for p, s in sorted(
                    slots.items(),
                    key=lambda item: runtime_names.get(item[0], item[0]),
                )
            )
            + "]"
        )
        factory = (
            f"Some({slot('process_factory')})"
            if service.get("processFactory", False)
            else "None"
        )
        result.append(
            f"ServiceDefinition{{context:{context},name:{quote(name)},elf:include_bytes!({quote(service['elf'])}),ram:{service['ram']},boot:{str(service['activation'] == 'boot').lower()},bootstrap:ProcessBootstrap{{factory:{factory},vm:{base['vm']},address_space:{base['address_space']},lifecycle:{base['lifecycle']},reply:{base['reply']}}},ready:{base['ready']},objects:&[{','.join(objects)}],requires:&[{requirements}],namespaces:&[{namespaces}],grants:&[{grants}],after:&[{after}],publish:&[{published_prefixes}]}},"
        )
    return "\n".join(result + ["];"])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("input", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.write_text(compile_services(json.loads(args.input.read_text())))


if __name__ == "__main__":
    main()
