default:
    @just --list

fmt:
    treefmt

build-sel4 profile="default":
    nix-build nix -A sel4.out --argstr profile '{{ profile }}' --out-link work/sel4

rust-generate:
    python3 tools/rust-generate.py

npins-bump:
    npins -d nix/npins update

check-idl:
    ./tools/idl/check.sh

demo:
    pytest -q tests/boot/test_idl_demo.py -s

# Interactive virtio console; kernel/debug output goes to work/horn-debug.log.
shell accel="kvm":
    #!/usr/bin/env bash
    set -euo pipefail
    case '{{ accel }}' in
      kvm)
        if [[ ! -r /dev/kvm || ! -w /dev/kvm ]]; then
          echo 'KVM requires access to /dev/kvm; use just shell tcg explicitly instead.' >&2
          exit 1
        fi
        machine=q35,kernel-irqchip=split
        cpu=host,migratable=off
        ;;
      tcg) machine=q35; cpu=max ;;
      *) echo 'Usage: just shell [kvm|tcg]' >&2; exit 1 ;;
    esac
    mkdir -p work
    mapfile -t images < <(nix-build nix -A sel4.out -A root-task.package \
      --argstr profile pc-dev --arg modules '[ ./examples/devmgr/console.nix ]' \
      --no-out-link)
    [[ ${#images[@]} == 2 ]]
    exec qemu-system-x86_64 -machine "$machine" -accel '{{ accel }}' -cpu "$cpu" -m 512 -smp 1 \
      -nodefaults -no-user-config -nic none -display none -monitor none -no-reboot \
      -global hpet.msi=on \
      -netdev user,id=net0,restrict=on,hostfwd=tcp:127.0.0.1:8080-:80 \
      -device virtio-net-pci,netdev=net0,addr=05.0,disable-legacy=on,iommu_platform=on,mac=52:54:00:12:34:56 \
      -serial file:work/horn-debug.log \
      -kernel "${images[0]}/bin/kernel-multiboot.elf" \
      -initrd "${images[1]}/bin/task.elf" \
      -append 'console_port=0x3f8 debug_port=0x3f8' \
      -device intel-iommu,intremap=on,caching-mode=on,device-iotlb=off,aw-bits=48 \
      -chardev stdio,id=console,mux=on,signal=off \
      -device virtio-serial-pci,id=vserial,addr=04.0,disable-legacy=on,iommu_platform=on \
      -device virtconsole,chardev=console,bus=vserial.0
