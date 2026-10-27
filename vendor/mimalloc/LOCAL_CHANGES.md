# Horn port changes

The revision/tree in `upstream.json` identify pristine mimalloc v3.6.0; upstream
files and its MIT license are retained. Local edits:

- `src/prim/prim.c` selects the external Horn primitive backend under MI_HORN.
- `include/mimalloc/atomic.h` avoids POSIX thread/lock dependencies for MI_HORN.
- `src/util/options.c` avoids FILE/stdout/stderr pointer comparisons under MI_HORN;
  diagnostics still use mimalloc's output callback and primitive stderr hook.

The backend and native build live in `runtime/mimalloc/`; the Rust VM hooks live
in `runtime/rust/horn-mimalloc/`. No sources are fetched during compilation.
