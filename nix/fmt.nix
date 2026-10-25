{pkgs ? import ./.}: let
  sources = import ./npins;
  treefmt-nix = import sources.treefmt-nix;
in
  treefmt-nix.mkWrapper pkgs {
    projectRootFile = "shell.nix";
    programs.ruff-format.enable = true;
    programs.just.enable = true;
    programs.alejandra.enable = true;
    programs.ruff-check.enable = true;
    programs.ruff-check.extendSelect = ["I"];
    settings.formatter.ruff-format.priority = 1;
    settings.global.excludes = [
      "vendor/**"
      "work/**"
      "nix/npins/**"
    ];
  }
