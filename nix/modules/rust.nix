{
  config,
  lib,
  ...
}:
{
  options.rust = {
    libhornHeaders = lib.mkOption {
      type = lib.types.package;
      readOnly = true;
      description = "libhorn C startup header generated and compile-checked on the build host.";
      default = import ../../platform/libhorn {
        pkgs = config.nixpkgs.pkgs.pkgsBuildBuild;
      };
    };
    release = lib.mkOption {
      type = lib.types.bool;
      default = false;
    };
    workspace = lib.mkOption {
      type = lib.types.raw;
      readOnly = true;
      default = lib.makeOverridable (import ../rust) {
        pkgs = config.sel4.pkgs;
        rust = config.toolchain.rust;
        sel4 = config.sel4.out;
        release = config.rust.release;
      };
    };
  };
}
