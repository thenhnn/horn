{
  config,
  lib,
  sources,
  ...
}: let
  cfg = config.nixpkgs;
  platformType = lib.types.either lib.types.str lib.types.attrs;
in {
  options.nixpkgs = {
    hostPlatform = lib.mkOption {
      type = platformType;
      apply = lib.systems.elaborate;
      description = "Platform that the packages run on.";
    };
    buildPlatform = lib.mkOption {
      type = platformType;
      default = builtins.currentSystem;
      apply = lib.systems.elaborate;
      description = "Platform that builds the packages.";
    };
    pkgs = lib.mkOption {
      type = lib.types.pkgs;
      default = assert lib.assertMsg cfg.buildPlatform.isLinux "Horn builds require a Linux host.";
        import sources.nixpkgs {
          localSystem = cfg.buildPlatform;
          crossSystem = cfg.hostPlatform;
        };
      description = "Package set supplied to modules as pkgs.";
    };
  };

  config._module.args.pkgs = cfg.pkgs;
}
