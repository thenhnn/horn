{
  config,
  lib,
  ...
}: {
  options.rust = {
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
