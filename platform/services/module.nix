{
  config,
  lib,
  pkgs,
  ...
}: let
  inherit (lib) mkOption types;
  requirement = types.submodule {
    options = {
      path = mkOption {type = types.str;};
      select = mkOption {
        type = types.enum ["exact" "first"];
        default = "exact";
      };
    };
  };
  namespace = types.submodule {
    options = {
      path = mkOption {type = types.str;};
      minReady = mkOption {
        type = types.enum [0 1];
        default = 0;
      };
    };
  };
  binding = types.submodule {
    options = {
      badge = mkOption {
        type = types.nullOr (types.oneOf [types.ints.unsigned types.str]);
        default = null;
      };
      rights = mkOption {type = types.ints.between 1 15;};
    };
  };
  object = types.submodule {
    options = {
      kind = mkOption {type = types.enum ["Endpoint" "Notification"];};
      bindings = mkOption {
        type = types.attrsOf binding;
        default = {};
      };
      bindNotification = mkOption {
        type = types.bool;
        default = false;
      };
    };
  };
  definition = types.submodule {
    options = {
      elf = mkOption {type = types.path;};
      bootstrap = mkOption {
        type = types.path;
        description = "IDL-generated named bootstrap slot descriptor.";
      };
      capset = mkOption {
        type = types.str;
        description = "Declaration name in the descriptor, not a service category.";
      };
      activation = mkOption {
        type = types.enum ["boot" "automatic"];
        default = "automatic";
      };
      ram = mkOption {type = types.ints.positive;};
      requires = mkOption {
        type = types.attrsOf requirement;
        default = {};
      };
      after = mkOption {
        type = types.listOf types.str;
        default = [];
      };
      namespaces = mkOption {
        type = types.attrsOf namespace;
        default = {};
      };
      objects = mkOption {
        type = types.attrsOf object;
        default = {};
      };
      exports = mkOption {
        type = types.listOf (types.attrsOf types.anything);
        default = [];
      };
      grants = mkOption {
        type = types.attrsOf (types.attrsOf types.anything);
        default = {};
      };
      publish = mkOption {
        type = types.listOf types.str;
        default = [];
        description = "Subtrees in which this owner's supervised children may publish.";
      };
    };
  };
in {
  options.services = {
    definitions = mkOption {
      type = types.attrsOf definition;
      default = {};
    };
    catalog = mkOption {
      type = types.path;
      readOnly = true;
    };
  };
  config.services.catalog = pkgs.pkgsBuildBuild.runCommand "horn-services.rs" {
    nativeBuildInputs = [pkgs.pkgsBuildBuild.python3];
    manifest = builtins.toJSON (lib.mapAttrs (_: s:
      s
      // {
        elf = toString s.elf;
        bootstrap = toString s.bootstrap;
      })
    config.services.definitions);
    passAsFile = ["manifest"];
  } ''python3 ${../../tools/services/compile.py} "$manifestPath" "$out"'';
}
