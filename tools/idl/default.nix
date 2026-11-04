{pkgs}: let
  inherit (pkgs) lib;
  manifest = builtins.fromJSON (builtins.readFile ./sources.json);
  root = ./. + "/${manifest.root}";
  pythonSource = path:
    lib.fileset.toSource {
      root = path;
      fileset = lib.fileset.fileFilter (file: file.hasExt "py") path;
    };
  sources = lib.mapAttrs (_: relative: let
    directory = pythonSource (root + "/${builtins.dirOf relative}");
  in "${directory}/${builtins.baseNameOf relative}")
  manifest.schemas;
  catalog = pkgs.writeText "horn-idl-sources.json" (builtins.toJSON {
    root = "/";
    schemas = sources;
  });
  compiler = pythonSource ./.;
  environment = {
    HORN_IDL = "${compiler}";
    HORN_IDL_SOURCES = "${catalog}";
    PYTHONPATH = "${compiler}";
  };
in {
  crate = old:
    environment
    // {
      nativeBuildInputs = (old.nativeBuildInputs or []) ++ [pkgs.python3];
    };
  reference = pkgs.runCommand "horn-idl-reference.json" (environment
    // {
      nativeBuildInputs = [pkgs.python3];
    }) ''python3 -B -m horn_idl --doc-json "$out"'';
  capsets = schema:
    assert builtins.hasAttr schema sources;
      pkgs.runCommand "horn-${schema}-capsets.json" (environment
        // {
          nativeBuildInputs = [pkgs.python3];
        }) ''python3 -B -m horn_idl ${lib.escapeShellArg schema} --capsets "$out"'';
}
