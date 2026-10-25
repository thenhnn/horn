# Vendoring

Requires Python 3.11+, Git and jj.

## Import

```sh
python3 tools/vendor.py add sel4 \
  --url https://github.com/seL4/seL4.git \
  --rev COMMIT_OR_TAG --dest vendor/sel4/src
python3 tools/vendor.py merge sel4
```

## Update

```sh
python3 tools/vendor.py update sel4 --rev NEW_COMMIT_OR_TAG
python3 tools/vendor.py merge sel4
jj resolve --list
```

Edit vendored sources directly. Resolve merge conflicts before publishing.

## Status

```sh
python3 tools/vendor.py status sel4
```
