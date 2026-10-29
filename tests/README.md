# tests

run inside `nix-shell`:

```sh
pytest -q
```

Linux bring-up smoke (requires `/dev/kvm`; skipped when unavailable):

```sh
pytest -q tests/boot/test_linux.py
```

The VM run has a 15-second deadline, independent of image build time and the
service's 30-second READY deadline. It checks PID 1, clock/sleep and two further
seconds without faults. TCG has not passed this bounded Linux boot test.

Focused network smoke (single-CPU and two-CPU TCG boots, not a profile matrix):

```sh
pytest -q tests/boot/test_network.py -s
```

It holds twelve incomplete HTTP requests open while other requests complete,
then exercises connection-handle retirement beyond the live quota. The guest
uses a single-threaded HTTP event loop; host test workers simulate clients.
