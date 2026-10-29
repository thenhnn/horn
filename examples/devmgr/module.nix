{
  config,
  lib,
  pkgs,
  ...
}: let
  # Embedded launch images need ELF/TLS/.horn metadata, not nested DWARF.
  # Keep original packages for symbolization; strip only the embedded copies.
  runtimeElf = package: name:
    pkgs.pkgsBuildBuild.runCommand "${name}-runtime.elf" {
      nativeBuildInputs = [pkgs.pkgsBuildBuild.llvmPackages.llvm];
    } ''llvm-objcopy --strip-debug ${package}/bin/${name}.elf "$out"'';
  idl = import ../../tools/idl {pkgs = pkgs.pkgsBuildBuild;};
  bootstrap = idl.capsets;
  driverCatalog = pkgs.pkgsBuildBuild.writeText "horn-drivers.rs" (''      static DRIVERS: &[DriverConfig] = &[
    ''
    + lib.concatMapStrings (d: let
      option = v:
        if v == null
        then "None"
        else "Some(${toString v})";
    in ''      DriverConfig{clock:${lib.boolToString d.clock},name:${builtins.toJSON d.name},ram:${toString d.ram},dma:horn_devmgr::dma::DmaPolicy{bytes:${toString d.dmaBytes},objects:${toString d.dmaObjects}},namespace:${builtins.toJSON d.publish.namespace},badge:${toString d.publish.badge},rights:${toString d.publish.rights},elf:include_bytes!(${builtins.toJSON (toString d.elf)}),rule:horn_devmgr::matching::PciMatch{vendor:${option (d.pci.vendor or null)},device:${option (d.pci.device or null)},class:${toString (d.pci.class or 0)},class_mask:${toString (d.pci.classMask or 0)}}},
    '')
    config.devmgr.drivers
    + "];\n");
  mimalloc = import ../../platform/runtime/mimalloc {pkgs = pkgs.pkgsBuildBuild;};
  console = import ../../platform/drivers/virtio/console {
    workspace = config.rust.workspace;
    pkgs = pkgs.pkgsBuildBuild;
    probe = config.devmgr-demo.dma-probe;
    driverTest =
      if config.devmgr-demo.stop-mode == "timeout"
      then "ignore"
      else config.devmgr-demo.stop-mode;
  };
  childWorkspace = config.rust.workspace.override {
    target = "sel4";
    crateOverrides = {
      memchr = old: {features = ["alloc"];};
      horn-devmgr-task = old: {
        features = (old.features or []) ++ lib.optional config.devmgr-demo.boot-tests "boot-tests";
        DEVMGR_TEST_FAULT =
          if config.devmgr-demo.inject-fault
          then "1"
          else "0";
        DEVMGR_TEST_EARLY_FAULT =
          if config.devmgr-demo.early-fault
          then "1"
          else "0";
        DEVMGR_TEST_NO_READY =
          if config.devmgr-demo.withhold-ready
          then "1"
          else "0";
        # QEMU HPET uses system-memory MSI, outside PCI DMA translation.
        HORN_CLOCK_ROUTE = "qemu-system-msi";
        DEVMGR_DRIVER_CATALOG = "${driverCatalog}";
        DEVMGR_PROBE_ELF =
          if config.devmgr-demo.boot-tests
          then "${runtimeElf childWorkspace.members.horn-devmgr-probe "devmgr_probe"}"
          else "";
      };
      horn-netd = old: {
        HORN_TCP_BUFFER_BYTES = toString config.devmgr-demo.tcp-buffer-bytes;
        HORN_TCP_CONNECTIONS = toString config.devmgr-demo.tcp-connections;
        HORN_TCP_BACKLOG = toString config.devmgr-demo.tcp-backlog;
      };
      horn-supervisor-task = old: {
        features = (old.features or []) ++ lib.optional config.devmgr-demo.boot-tests "boot-tests";
        HORN_SERVICES = "${config.services.catalog}";
        SUPERVISOR_STALL =
          if config.devmgr-demo.stall-bootstrap
          then "1"
          else "0";
      };
      horn-mimalloc = old: {MIMALLOC_LIB = "${mimalloc}/lib";};
    };
  };
  rootWorkspace = config.rust.workspace.override {
    target = "sel4-roottask";
    crateOverrides = {
      devmgr-demo-root = old: {
        features = (old.features or []) ++ lib.optional config.devmgr-demo.boot-tests "boot-tests";
        SUPERVISOR_ELF = "${runtimeElf childWorkspace.members.horn-supervisor-task "supervisor"}";
        DEVMGR_TEST_VTD =
          if config.devmgr.drivers != []
          then "1"
          else "0";
        DEVMGR_TEST_STALL =
          if config.devmgr-demo.stall-bootstrap
          then "1"
          else "0";
        ROOT_STARTUP_TIMEOUT_US = toString config.devmgr-demo.startup-timeout-us;
      };
      memchr = old: {features = ["alloc"];};
    };
  };
in {
  imports = [../../platform/services/module.nix];
  options.devmgr.drivers = lib.mkOption {
    type = lib.types.listOf (lib.types.submodule {
      options = {
        name = lib.mkOption {type = lib.types.str;};
        clock = lib.mkOption {type = lib.types.bool; default = false;};
        elf = lib.mkOption {type = lib.types.path;};
        ram = lib.mkOption {type = lib.types.ints.positive;};
        dmaBytes = lib.mkOption {
          type = lib.types.ints.between 4096 (64 * 1024 * 1024);
          default = 12 * 65536;
        };
        dmaObjects = lib.mkOption {
          type = lib.types.ints.between 1 64;
          default = 12;
        };
        pci = lib.mkOption {type = lib.types.attrsOf lib.types.int;};
        publish = lib.mkOption {type = lib.types.attrsOf lib.types.anything;};
      };
    });
    default = [];
  };
  options.devmgr-demo.boot-tests = lib.mkOption {
    type = lib.types.bool;
    default = false;
    description = "Compile boot-test fixtures and self-checks; never enabled for ordinary boots.";
  };
  options.devmgr-demo.early-fault = lib.mkOption {
    type = lib.types.bool;
    default = false;
  };
  options.devmgr-demo.withhold-ready = lib.mkOption {
    type = lib.types.bool;
    default = false;
  };
  options.devmgr-demo.startup-timeout-us = lib.mkOption {
    type = lib.types.ints.positive;
    default = 30000000;
  };
  options.devmgr-demo.stop-mode = lib.mkOption {
    type = lib.types.enum ["none" "orderly" "force" "timeout" "fault" "exit"];
    default = "none";
    description = "Isolated supervisor/driver lifecycle test mode; never enables reassignment.";
  };
  options.devmgr-demo.dma-probe = lib.mkOption {
    type = lib.types.enum ["none" "outside" "stale"];
    default = "none";
  };
  options.devmgr-demo.tcp-buffer-bytes = lib.mkOption {
    type = lib.types.ints.between 16384 (16 * 1024 * 1024);
    default = 4 * 1024 * 1024;
    description = "Aggregate TCP RX/TX buffer budget; allocated on demand in netd's RAM account.";
  };
  options.devmgr-demo.tcp-connections = lib.mkOption {
    type = lib.types.ints.between 1 32;
    default = 32;
    description = "Accepted connection capability quota within netd's fixed CSpace partition.";
  };
  options.devmgr-demo.tcp-backlog = lib.mkOption {
    type = lib.types.ints.between 1 128;
    default = 32;
    description = "Pending handshake/accept sockets per listening port.";
  };
  options.devmgr-demo.net = lib.mkOption {
    type = lib.types.bool;
    default = false;
  };
  options.devmgr-demo.shell = lib.mkOption {
    type = lib.types.bool;
    default = config.devmgr-demo.console;
  };
  options.devmgr-demo.console = lib.mkOption {
    type = lib.types.bool;
    default = false;
  };
  options.devmgr-demo.inject-fault = lib.mkOption {
    type = lib.types.bool;
    default = false;
  };
  options.devmgr-demo.stall-bootstrap = lib.mkOption {
    type = lib.types.bool;
    default = false;
  };
  config = {
    devmgr.drivers =
      (lib.optional config.devmgr-demo.console {
        name = "virtio-console";
        clock = config.devmgr-demo.dma-probe != "none";
        elf = runtimeElf console "virtio_console";
        ram = 32 * 1024 * 1024;
        pci = {
          vendor = 6900;
          device = 4163;
        }; # 1af4:1043, modern virtio console
        publish = {
          namespace = "/consoles/virtio";
          badge = 16;
          rights = 14;
        };
      })
      ++ (lib.optional config.devmgr-demo.net {
        name = "virtio-net";
        elf = runtimeElf childWorkspace.members.horn-virtio-net "virtio_net";
        ram = 32 * 1024 * 1024;
        pci = {
          vendor = 6900;
          device = 4161;
        }; # 1af4:1041, modern virtio network
        publish = {
          namespace = "/net/ethernet/virtio";
          badge = 16;
          rights = 14;
        };
      });
    services.definitions = {
      devmgr = {
        elf = runtimeElf childWorkspace.members.horn-devmgr-task "devmgr";
        bootstrap = bootstrap "horn_device";
        capset = "DeviceCaps";
        activation = "boot";
        ram = 64 * 1024 * 1024;
        grants = {
          platform = {
            source = "Platform";
            rights = 15;
          };
          supervisor = {
            source = "SupervisorFactory";
            rights = 15;
          };
        };
        namespaces.clocks = {path = "/clocks"; minReady = 0;};
        publish = ["/consoles"] ++ lib.optional config.devmgr-demo.net "/net/ethernet";
        objects = {
          service = {
            kind = "Endpoint";
            bindings.service.rights = 15;
          };
          events = {
            kind = "Notification";
            bindNotification = true;
            bindings = {
              fault = {
                badge = "0x8000000000000000";
                rights = 2;
              };
              timer = {
                badge = 2305843009213693952;
                rights = 2;
              };
              events = {
                badge = 4611686018427387904;
                rights = 2;
              };
            };
          };
        };
        exports = [
          {
            object = "service";
            path = "/services/devmgr";
            badge = 1;
            rights = 10;
          }
          {
            object = "service";
            path = "/devices/platform/clock";
            badge = 1152921504606846976;
            rights = 14;
          }
        ];
      };
      timerd = {
        elf = runtimeElf childWorkspace.members.horn-timerd "timerd";
        bootstrap = bootstrap "horn_clock";
        capset = "TimerCaps";
        ram = 8 * 1024 * 1024;
        requires.source = {path = "/devices/platform/clock";};
        objects = {
          service = {
            kind = "Endpoint";
            bindings.service.rights = 15;
          };
          events = {
            kind = "Notification";
            bindNotification = true;
            bindings = {
              events.rights = 1;
              signal = {
                badge = 1;
                rights = 2;
              };
            };
          };
        };
        exports = [
          {
            object = "service";
            path = "/services/clock";
            badge = 16;
            rights = 14;
          }
          {
            object = "service";
            path = "/clocks/monotonic";
            badge = 16;
            rights = 14;
          }
        ];
      };
      netd = lib.mkIf config.devmgr-demo.net {
        elf = runtimeElf childWorkspace.members.horn-netd "netd";
        bootstrap = bootstrap "horn_net";
        capset = "NetdCaps";
        ram = 32 * 1024 * 1024;
        requires = {
          ethernet = {
            path = "/net/ethernet";
            select = "first";
          };
          clock = {path = "/services/clock";};
        };
        objects = {
          service = {
            kind = "Endpoint";
            bindings.service.rights = 15;
          };
          events = {
            kind = "Notification";
            bindNotification = true;
            bindings = {
              events.rights = 1;
              ethernet_signal = {
                badge = 1;
                rights = 2;
              };
              timer_signal = {
                badge = 2;
                rights = 2;
              };
            };
          };
        };
        exports = [
          {
            object = "service";
            path = "/services/net/tcp";
            badge = 16;
            rights = 14;
          }
        ];
      };
      http-demo = lib.mkIf config.devmgr-demo.net {
        elf = runtimeElf childWorkspace.members.horn-http-demo "http_demo";
        bootstrap = bootstrap "horn_net";
        capset = "HttpCaps";
        ram = 8 * 1024 * 1024;
        requires.tcp = {path = "/services/net/tcp";};
        objects.events = {
          kind = "Notification";
          bindings = {
            events.rights = 1;
            signal.rights = 2;
          };
        };
      };
      horn-shell = lib.mkIf config.devmgr-demo.shell {
        elf = runtimeElf childWorkspace.members.horn-shell "shell";
        bootstrap = bootstrap "horn_shell";
        capset = "ShellCaps";
        ram = 8 * 1024 * 1024;
        requires = {
          console = {
            path = "/consoles";
            select = "first";
          };
          pci = {path = "/services/devmgr";};
          processes = {path = "/services/procd";};
        };
        namespaces.consoles = {
          path = "/consoles";
          minReady = 1;
        };
        namespaces.registry = {
          path = "/";
          minReady = 0;
        };
        objects.events = {
          kind = "Notification";
          bindings = {
            events.rights = 1;
            signal = {
              badge = 1;
              rights = 2;
            };
          };
        };
      };
    };
    sel4.settings.KernelRootCNodeSizeBits = lib.mkOverride 900 16;
    root-task.package = lib.mkForce rootWorkspace.members.devmgr-demo-root;
  };
}
