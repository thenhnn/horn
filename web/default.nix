{pkgs ? import (import ../nix/npins).nixpkgs {}}: let
  idl = import ../tools/idl {inherit pkgs;};
  python = pkgs.python3.withPackages (packages: [packages.jinja2]);
  assets = pkgs.lib.fileset.toSource {
    root = ./assets;
    fileset = pkgs.lib.fileset.fileFilter (file: file.hasExt "svg") ./assets;
  };
in
  pkgs.runCommand "horn-website" {
    nativeBuildInputs = [python];
  } ''
    mkdir -p "$out/assets" "$out/doc/idl"
    cp ${assets}/*.svg "$out/assets/"
    cp ${./style.css} "$out/style.css"
    cp ${./docs.css} "$out/docs.css"
    python3 -B ${./render.py} ${./templates} ${idl.reference} "$out"
  ''
