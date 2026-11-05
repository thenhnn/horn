{
  config,
  lib,
  pkgs,
  ...
}: let
  linux = import ./. {
    pkgs = pkgs.pkgsBuildBuild;
    sources = import ../../nix/npins;
    sel4 = config.sel4.out;
    workspace = config.rust.workspace;
  };
in {
  imports = [../system/module.nix];
  # Root's bounded CSpace must cover resident Linux RAM and frame aliases.
  sel4.settings.KernelRootCNodeSizeBits = 18;
  devmgr-demo.console = true;
  devmgr-demo.shell = false;
  devmgr-demo.root-metadata-bytes = 32 * 1024 * 1024;
  services.definitions.linux = {
    elf = "${linux}/bin/task.elf";
    ram = 64 * 1024 * 1024;
    processFactory = true;
    requires = {
      console = {
        path = "/consoles";
        select = "first";
      };
      clock = {path = "/services/clock";};
    };
    objects.events = {
      kind = "Notification";
      bindings = {
        events.rights = 1;
        user_signal = {
          badge = 1;
          rights = 2;
        };
        clock_signal = {
          badge = 2;
          rights = 2;
        };
        console_signal = {
          badge = 4;
          rights = 2;
        };
      };
    };
  };
}
