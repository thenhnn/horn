"""Import upstream source and merge updates using jj."""

import argparse
import contextlib
import fcntl
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path, PurePosixPath

MANIFEST = "upstream.json"


def run(root, *args):
    result = subprocess.run(args, cwd=root, capture_output=True, check=False)
    if result.returncode:
        raise RuntimeError(result.stderr.decode(errors="replace").strip())
    return result.stdout.decode(errors="surrogateescape").strip()


def jj(root, *args):
    return run(root, "jj", "--color=never", "--no-pager", *args)


def identity(root, revision):
    ids = jj(
        root, "log", "--no-graph", "-r", revision, "-T", 'commit_id ++ "\n"'
    ).splitlines()
    if len(ids) != 1:
        raise ValueError(f"expected one revision: {revision}")
    return ids[0]


def destination(root, raw):
    path = PurePosixPath(raw)
    if (
        path.is_absolute()
        or str(path) == "."
        or any(part in ("..", ".git", ".jj", "work") for part in path.parts)
    ):
        raise ValueError(
            "destination must be a relative source directory outside work/.git/.jj"
        )
    current = root
    for part in path.parts:
        current /= part
        if current.is_symlink():
            raise ValueError(f"destination traverses a symlink: {current}")
    return Path(str(path))


def manifest(root, revision, name):
    matches = []
    for filename in jj(root, "file", "list", "-r", revision).splitlines():
        if PurePosixPath(filename).name == MANIFEST:
            meta = json.loads(jj(root, "file", "show", "-r", revision, filename))
            if meta["name"] == name:
                matches.append((Path(filename), meta))
    if not matches:
        raise LookupError(f"no manifest for {name} in {revision}")
    if len(matches) != 1:
        raise ValueError(f"multiple manifests for {name}")
    path, meta = matches[0]
    if (
        meta["schema"] != 1
        or not meta["repository"]
        or not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", meta["revision"])
        or not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", meta["tree"])
        or meta["destination"] in (".", "..", MANIFEST)
        or PurePosixPath(meta["destination"]).parts != (meta["destination"],)
    ):
        raise ValueError(f"invalid manifest: {path}")
    destination(root, str(path.parent / meta["destination"]))
    return path, meta


def validate_import(root, revision, name):
    path, meta = manifest(root, revision, name)
    source = path.parent / meta["destination"]
    git_dir = jj(root, "git", "root")
    tree = run(root, "git", "--git-dir", git_dir, "rev-parse", f"{revision}:{source}")
    if tree != meta["tree"]:
        raise ValueError(f"{revision} is not a pristine upstream import")
    return path, meta


def anchor(root, name):
    ref = f"vendor/{name}/upstream"
    bookmarks = jj(root, "bookmark", "list", "-T", 'name ++ "\n"').splitlines()
    if ref in bookmarks:
        revision = identity(root, ref)
    else:
        path, meta = manifest(root, "@", name)
        marker = json.dumps(f"x-horn-vendor: {name} {meta['revision']} {path}")
        revision = identity(root, f"ancestors(@) & description(substring:{marker})")
    path, meta = validate_import(root, revision, name)
    try:
        current_path, current = manifest(root, "@", name)
    except LookupError:
        return revision
    if (path, meta["repository"], meta["destination"]) != (
        current_path,
        current["repository"],
        current["destination"],
    ):
        raise ValueError("upstream import and development manifest do not match")
    return revision


@contextlib.contextmanager
def locked(root, name):
    if not re.fullmatch(r"[A-Za-z][A-Za-z0-9_-]*", name):
        raise ValueError(
            "name must start with a letter; use letters, digits, '_' or '-'"
        )
    cache = root / "work" / "vendor" / name
    cache.mkdir(parents=True, exist_ok=True)
    with (cache / "import.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield cache


def fetch(cache, url, revision):
    if not url or url.startswith("-") or not revision or revision.startswith("-"):
        raise ValueError("repository and revision cannot be empty or start with '-'")
    repo = cache / "upstream.git"
    if not repo.exists():
        run(cache, "git", "init", "--bare", str(repo))
        run(cache, "git", "--git-dir", str(repo), "remote", "add", "origin", url)
    if run(cache, "git", "--git-dir", str(repo), "remote", "get-url", "origin") != url:
        raise ValueError("cached upstream has a different URL")
    git = ["git", "--git-dir", str(repo)]
    available = (
        re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", revision)
        and subprocess.run(
            [*git, "cat-file", "-e", f"{revision}^{{commit}}"],
            capture_output=True,
            check=False,
        ).returncode
        == 0
    )
    if not available:
        print(f"Fetching {url} at {revision}")
        run(cache, *git, "fetch", "--depth=1", "--no-tags", "origin", revision)
        revision = run(cache, *git, "rev-parse", "FETCH_HEAD^{commit}")
    run(cache, *git, "update-ref", f"refs/horn/{revision}", revision)
    return repo, revision, run(cache, *git, "rev-parse", f"{revision}^{{tree}}")


def export_tree(repo: Path, revision: str, out: Path) -> None:
    git = ["git", "--git-dir", str(repo)]
    listing = subprocess.run(
        [*git, "ls-tree", "-rz", revision], check=True, capture_output=True
    ).stdout
    entries = []
    for entry in listing.split(b"\0"):
        if not entry:
            continue
        header, raw_path = entry.split(b"\t", 1)
        mode, kind, oid = header.split()
        path = PurePosixPath(os.fsdecode(raw_path))
        if kind != b"blob" or mode not in (b"100644", b"100755", b"120000"):
            raise ValueError(
                f"unsupported upstream entry (including submodules): {path}"
            )
        if path.is_absolute() or any(p in ("..", ".git", ".jj") for p in path.parts):
            raise ValueError(f"unsafe upstream path: {path}")
        entries.append((mode, oid, Path(str(path))))
    out.mkdir(parents=True)
    proc = subprocess.Popen(
        [*git, "cat-file", "--batch"], stdin=subprocess.PIPE, stdout=subprocess.PIPE
    )
    try:
        assert proc.stdin is not None and proc.stdout is not None
        for mode, oid, path in entries:
            proc.stdin.write(oid + b"\n")
            proc.stdin.flush()
            header = proc.stdout.readline().split()
            if len(header) != 3 or header[0] != oid or header[1] != b"blob":
                raise ValueError(f"could not read upstream blob: {path}")
            size = int(header[2])
            target = out / path
            target.parent.mkdir(parents=True, exist_ok=True)
            if mode == b"120000":
                data = proc.stdout.read(size)
                if len(data) != size:
                    raise ValueError("truncated upstream blob")
                link = os.fsdecode(data)
                if os.path.isabs(link) or not (
                    target.parent / link
                ).resolve().is_relative_to(out.resolve()):
                    raise ValueError(f"upstream symlink escapes source tree: {path}")
                target.symlink_to(link)
            else:
                with target.open("wb") as stream:
                    remaining = size
                    while remaining:
                        data = proc.stdout.read(min(remaining, 1024 * 1024))
                        if not data:
                            raise ValueError("truncated upstream blob")
                        stream.write(data)
                        remaining -= len(data)
                target.chmod(0o755 if mode == b"100755" else 0o644)
            if proc.stdout.read(1) != b"\n":
                raise ValueError("invalid upstream blob framing")
        proc.stdin.close()
        if proc.wait():
            raise ValueError("upstream blob export failed")
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()
        if proc.stdin and not proc.stdin.closed:
            proc.stdin.close()
        if proc.stdout:
            proc.stdout.close()


def import_tree(root, cache, name, base, path, meta, source):
    workspace = cache / "workspace"
    workspace_name = f"horn-vendor-{name}"
    jj(
        root,
        "workspace",
        "add",
        "--name",
        workspace_name,
        "--sparse-patterns=empty",
        "-r",
        base,
        str(workspace),
    )
    try:
        jj(workspace, "sparse", "set", "--clear", "--add", str(path.parent))
        dest = workspace / path.parent / meta["destination"]
        if dest.exists():
            shutil.rmtree(dest)
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copytree(source, dest, symlinks=True)
        (workspace / path).write_text(json.dumps(meta, indent=2) + "\n")
        jj(
            workspace,
            "file",
            "track",
            "--include-ignored",
            str(dest.relative_to(workspace)),
            str(path),
        )
        jj(
            workspace,
            "describe",
            "-m",
            f"Import vendor {name}\n\nx-horn-vendor: {name} {meta['revision']} {path}",
        )
        imported = identity(workspace, "@")
        validate_import(workspace, imported, name)
        jj(root, "bookmark", "set", f"vendor/{name}/upstream", "-r", imported)
        return imported
    finally:
        jj(root, "workspace", "forget", workspace_name)
        shutil.rmtree(workspace)


def prepare(root, cache, args):
    if args.command == "add":
        base = identity(root, "@")
        source_path = destination(root, args.dest)
        path = source_path.parent / MANIFEST
        if (root / source_path).exists() or (root / path).exists():
            raise ValueError(
                "initial import cannot overwrite an existing source or manifest"
            )
        refs = jj(root, "bookmark", "list", "-T", 'name ++ "\n"').splitlines()
        if f"vendor/{args.name}/upstream" in refs:
            raise ValueError("vendor import bookmark already exists")
        meta = {
            "schema": 1,
            "name": args.name,
            "repository": args.url,
            "destination": source_path.name,
        }
    else:
        base = anchor(root, args.name)
        path, meta = validate_import(root, base, args.name)
    repo, revision, tree = fetch(cache, meta["repository"], args.rev)
    if revision == meta.get("revision"):
        print(f"{args.name} already imports {revision}")
        return
    with tempfile.TemporaryDirectory(dir=cache, prefix="export-") as temporary:
        source = Path(temporary) / "src"
        export_tree(repo, revision, source)
        imported = import_tree(
            root,
            cache,
            args.name,
            base,
            path,
            dict(meta, revision=revision, tree=tree),
            source,
        )
    print(
        f"Imported {args.name}: {revision}\njj revision: {imported}\n"
        f"Merge explicitly: python3 tools/vendor.py merge {args.name}"
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    add = commands.add_parser(
        "add", help="Prepare an import without changing development files."
    )
    add.add_argument("name")
    add.add_argument("--url", required=True)
    add.add_argument("--rev", required=True)
    add.add_argument("--dest", required=True)
    update = commands.add_parser(
        "update", help="Prepare an upstream update without merging it."
    )
    update.add_argument("name")
    update.add_argument("--rev", required=True)
    for command in ("merge", "status"):
        commands.add_parser(command).add_argument("name")
    args = parser.parse_args()
    root = Path(run(Path.cwd(), "jj", "--ignore-working-copy", "root"))
    with locked(root, args.name) as cache:
        if args.command in ("add", "update"):
            prepare(root, cache, args)
        else:
            imported = anchor(root, args.name)
            if args.command == "status":
                path, meta = validate_import(root, imported, args.name)
                print(
                    f"Manifest: {path}\nRepository: {meta['repository']}\n"
                    f"Upstream: {meta['revision']}\nPristine jj revision: {imported}"
                )
            else:
                current = identity(root, "@")
                if jj(
                    root,
                    "log",
                    "--no-graph",
                    "-r",
                    f"{imported} & ancestors({current})",
                    "-T",
                    "commit_id",
                ):
                    print("Import is already an ancestor of the working copy")
                else:
                    jj(
                        root,
                        "new",
                        current,
                        imported,
                        "-m",
                        f"Merge vendor {args.name}",
                    )
                    print(jj(root, "status"))


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, ValueError, LookupError, OSError) as error:
        sys.exit(f"vendor: {error}")
