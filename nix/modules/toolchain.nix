{
  lib,
  pkgs,
  sources,
  ...
}: let
  build = pkgs.pkgsBuildBuild.extend (import sources.rust-overlay);
in {
  options.toolchain.rust = lib.mkOption {
    type = lib.types.package;
    readOnly = true;
    default = build.rust-bin.nightly.latest.minimal.override {
      extensions = ["rust-src" "rustfmt" "clippy" "rust-analyzer"];
    };
  };
}
