{pkgs}: let
  sources = import ./npins;
  treefmt-nix = import sources.treefmt-nix;
in
  treefmt-nix.mkWrapper pkgs {
    projectRootFile = "shell.nix";
    programs.ruff-format.enable = true;
    programs.just.enable = true;
    programs.alejandra.enable = true;
    programs.djlint = {
      enable = true;
      indent = 2;
      includes = ["web/templates/*.html"];
    };
    # djlint otherwise exits 1 after successfully reformatting files.
    settings.formatter.djlint.options = ["--profile=jinja" "--warn"];
    programs.ruff-check.enable = true;
    programs.ruff-check.extendSelect = ["I"];
    settings.formatter.ruff-format.priority = 1;
    settings.global.excludes = [
      "vendor/**"
      "work/**"
      "nix/npins/**"
      "Cargo.nix"
      "nix/rust/sysroot.nix"
      "nix/rust/cfg.json"
    ];
  }
