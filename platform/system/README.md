# System deployment

The working PC system composition and native seL4 root task live here, not in
examples. The device manager itself lives in `platform/core/devmgr`.

- `module.nix`: system services, driver catalog, bootstrap policy and root package.
- `console.nix`: development console, shell and network composition.
- `root/`: `horn-system-root`, privileged native seL4 bootstrap/VM process host.

Run `just shell`, or build:

```sh
nix-build nix -A root-task.package --argstr profile pc-dev \
  --arg modules '[ ./platform/system/console.nix ]' --no-out-link
```

Existing `devmgr-demo.*` test/deployment options retain their names for now;
this move does not change startup ordering, capability policy or the ABI.
