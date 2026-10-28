# tests

run inside `nix-shell`:

```sh
pytest -q
```

Focused network smoke (one TCG boot, not a profile matrix):

```sh
pytest -q tests/boot/test_network.py -s
```

It holds twelve incomplete HTTP requests open while other requests complete,
then exercises connection-handle retirement beyond the live quota. The guest
uses a single-threaded HTTP event loop; host test workers simulate clients.
