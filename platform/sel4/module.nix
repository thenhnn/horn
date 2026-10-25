{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.sel4;
  platform = import (./platforms + "/${cfg.platform}.nix");
in {
  imports = [./profiles/common.nix];

  options.sel4 = {
    platform = lib.mkOption {
      type = lib.types.enum (map (lib.removeSuffix ".nix") (builtins.attrNames (builtins.readDir ./platforms)));
      default = "pc";
      description = "Target board for the seL4 kernel.";
    };
    settings = lib.mkOption {
      type = lib.types.attrsOf (lib.types.oneOf [lib.types.bool lib.types.int lib.types.str]);
      default = {};
      description = "seL4 CMake configuration.";
    };
    out = lib.mkOption {
      type = lib.types.package;
      readOnly = true;
      description = "Kernel and matching libsel4 SDK.";
    };
  };

  config.sel4 = {
    settings =
      (lib.mapAttrs (_: lib.mkDefault) platform)
      // {
        KernelSel4Arch = pkgs.stdenv.hostPlatform.parsed.cpu.name;
      };
    out = import ./. {
      inherit pkgs;
      inherit (cfg) settings;
    };
  };
}
