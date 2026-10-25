{profile ? "default"}: let
  sources = import ./npins;
  lib = import (sources.nixpkgs + "/lib");
  tracked = ../platform/profiles + "/${profile}.nix";
  local = ../work/extra-profiles + "/${profile}.nix";
  selected =
    if builtins.pathExists tracked
    then tracked
    else if builtins.pathExists local
    then local
    else throw "Unknown profile '${profile}': expected platform/profiles/${profile}.nix or work/extra-profiles/${profile}.nix";
  evaluated = lib.evalModules {
    specialArgs = {inherit sources;};
    modules = [./modules/nixpkgs.nix ../platform/sel4/module.nix selected];
  };
in
  evaluated.config
  // {
    shell = import ./shell.nix {pkgs = evaluated.config.nixpkgs.pkgs.pkgsBuildBuild;};
  }
