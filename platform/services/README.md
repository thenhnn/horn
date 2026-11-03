# Services, dependencies and the registry

The initial configuration lives in `platform/system/module.nix`. It uses
`services.definitions` from `platform/services/module.nix`.

There are no `shell`, `devmgr` or `pci-driver` ABI categories in the launcher.
All services use ABI2. The configuration compiler assigns private placement
slots and emits a static Rust catalog plus named context bindings. Adding an
ordinary service does not require changing the compiler or supervisor.

## Service definition

Each definition supplies:

- `elf` and `ram`; obsolete `abi`, `bootstrap`, and `capset` options are rejected;
- `activation`: `boot` for the one root-critical bootstrap service, otherwise
  `automatic`;
- `requires.<port> = { path; select = "exact" | "first"; }`;
- `namespaces.<port> = { path; minReady = 0 | 1; }`;
- `after = [ paths ... ]`: startup ordering without a capability or lifetime edge;
- `objects`: process-owned endpoints/notifications and their named-slot bindings;
- `exports`: paths for configured endpoints, including badge and rights;
- privileged bootstrap `grants` and allowed child-publication prefixes `publish`.

The common task ports are `vm`, `address_space`, `lifecycle`, `reply`, and `ready`.
Other ports are deployment-defined names. Duplicate/reserved bindings and static
dependency cycles are rejected during catalog generation.

Services can set `logging = "/services/log"`; the compiler adds a dependency
and private `horn.log` grant. Runtime installs the synchronous logger before main,
with a dedicated transport disjoint from application and heap receive slots. The
logger itself keeps `logging = null` to avoid recursion/dependency cycles.

The compiler assigns
private slots to the common ports followed by bytewise-sorted deployment ports;
no slot constants are generated for the application. Common ports appear in its
context as `horn.vm`, `horn.address-space`, `horn.lifecycle`, `horn.reply` and
`horn.ready`. Application ports must be printable ASCII and must not shadow the
common ports or reserved `horn.*` namespace. The supervisor sends a sealed
names/selectors descriptor after all installs and before start; procd constructs
the actual startup record. Root-critical boot services use the same record and
cannot wait for runtime dependencies or READY-gated namespaces. Devmgr derives
its own log sink through supervisor after logd activation to avoid a boot cycle.

`requires` copies a published endpoint into the named input. `first` selects one
currently available endpoint in the subtree, once, in publication order. It does
not silently reconnect the process later. `namespaces` supplies a read-only
scoped directory capability; it does not bind the consumer to every provider.
`minReady = 1` waits for at least one available endpoint below the directory.
Missing runtime providers leave a service waiting, with a diagnostic log rather
than a spin loop. Interface compatibility is deliberately not checked.

The current shell definition binds one console for input and receives directory
access both to `/consoles` and to the whole public registry for inspection.

## Registry

The supervisor hosts a bounded registry module. A name selects a record; only a
kernel-badged directory capability authorizes access. Client paths are relative
to that directory. Absolute paths, `..`, empty components and backslash traversal
are rejected. `lookup` never launches a process.

API: paginated `list(after, recursive)`, endpoint `lookup(path)`, scoped
subdirectory `open(path)`, and one-shot `watch(notification, generation)`.
List results include a generation; recheck/rearm watches to avoid missed changes.
An opened/copied directory grant has one shared watcher. Publication is not a
right of a directory reader: supervised owners use their separate job authority,
within their configured publication prefixes.

Exports are declared before start and become available only after explicit
READY. Paths contain no interface-type strings. Withdrawal removes availability,
not already distributed endpoint capabilities. Existing owner-acknowledged
cleanup is preserved; bound ordinary consumers stop before device-owner cleanup.
Namespace discovery alone is not a hardware-lifetime guarantee. No restart,
reassignment or transparent recovery is introduced here.

Initial bounds: 8 supervised jobs, 64 namespace nodes and 32 directory grants
per boot. Records/grant badges are not recycled. `/services/procd` is a bootstrap
import of the existing root-hosted, supervisor-scoped process observer.

## PCI activation

Devmgr owns a separate embedded `devmgr.drivers` catalog: ELF, RAM budget,
`vendor`/`device` and/or packed 24-bit `class`/`classMask`, and publication policy.
No configured BDF selects a driver. Discovery selects candidates; assignment
still requires the actual scoped device capability. Unmatched devices are skipped; ambiguous matches produce a diagnostic.

Devmgr passes the selected ELF to supervisor, installs the device/IRQ capabilities,
declares the public endpoint, then starts the instance. Supervisor handles process
supervision; devmgr retains device isolation, DMA drain and cleanup ownership.
Shell startup is no longer devmgr policy.

The modern virtio console rule matches `1af4:1043` and publishes one endpoint per
matching PCI function:

```
/consoles/virtio/0000:00:04.0
/services/devmgr
/services/procd
```

In shell, `registry` dumps published paths and availability, `consoles` lists the
console subtree, and `ps` lists processes. Start with
`nix-shell --run 'just shell'` (KVM), or `just shell tcg` inside the Nix shell.
