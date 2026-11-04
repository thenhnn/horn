{pkgs}:
pkgs.runCommand "libhorn-startup-headers"
{
  nativeBuildInputs = [
    pkgs.rust-cbindgen
    pkgs.stdenv.cc
  ];
}
''
  mkdir -p $out/include/horn
  cbindgen --config ${./cbindgen.toml} ${./src/startup.rs} \
    --output $out/include/horn/startup.h
  $CC -std=c11 -Wall -Wextra -Werror -I$out/include \
    -fsyntax-only ${./header-check.c}
''
