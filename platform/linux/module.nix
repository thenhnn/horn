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
  };
in {
  imports = [../system/module.nix];
  # Root's bounded CSpace must cover resident Linux RAM and frame aliases.
  sel4.settings.KernelRootCNodeSizeBits = 18;
  devmgr-demo.console = true;
  devmgr-demo.shell = false;
  services.definitions.linux = {
    elf = "${linux}/bin/task.elf";
    bootstrap = "${linux}/caps.json";
    capset = "LinuxCaps";
    ram = 64 * 1024 * 1024;
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
