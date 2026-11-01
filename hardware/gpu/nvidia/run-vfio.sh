#!/usr/bin/env bash
# Rebuild the opt-in AD106 boot profile and launch QEMU with the GPU passed
# through to the guest.
#
# This script only BUILDS and RUNS QEMU. It never touches host driver state:
# no modprobe, no unbind/bind, no driver_override, no PCI reset, no VFIO
# re-binding. Binding the GPU and its audio function to vfio-pci is a separate,
# explicitly authorized host operation done by the operator.
#
# Usage:
#   hardware/gpu/nvidia/run-vfio.sh              # build + run
#   DRY_RUN=1 hardware/gpu/nvidia/run-vfio.sh    # build + print command
#
# Environment:
#   GPU=0000:01:00.0     GPU function passed to the guest
#   AUDIO=0000:01:00.1   sibling function, checked but NOT passed
#   MEM=1024             guest RAM in MiB
#   CPUS=1               guest vCPUs
#   MODULE=<path>        Nix module override (e.g. display-core.nix beside this script)
#   PROFILE=pc-dev       seL4 profile
#   QEMU=<path>          qemu-system-x86_64 override
#   PRIV=<cmd>           elevation command for QEMU only (default: doas, then sudo)
#   OUT_DIR=<path>       log directory (default: <repo>/work)
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
module=${MODULE:-$repo_root/hardware/gpu/nvidia/module.nix}
gpu=${GPU:-0000:01:00.0}
audio=${AUDIO:-0000:01:00.1}
mem=${MEM:-1024}
cpus=${CPUS:-1}
profile=${PROFILE:-pc-dev}
out_dir=${OUT_DIR:-$repo_root/work}
dry_run=${DRY_RUN:-0}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

note() {
    printf '%s\n' "$*" >&2
}

pci_driver() {
    local path=/sys/bus/pci/devices/$1/driver
    if [[ -L $path ]]; then
        basename "$(readlink -f "$path")"
    else
        printf 'none\n'
    fi
}

# ---------------------------------------------------------------- preflight
[[ -e /sys/bus/pci/devices/$gpu ]] || die "no such PCI device: $gpu"
[[ -e /sys/bus/pci/devices/$audio ]] || die "no such PCI device: $audio"
[[ -c /dev/kvm ]] || die "/dev/kvm missing: KVM is required for this profile"
[[ -d $repo_root/nix ]] || die "not a Horn checkout: $repo_root"

gpu_driver=$(pci_driver "$gpu")
audio_driver=$(pci_driver "$audio")
[[ $gpu_driver == vfio-pci ]] ||
    die "$gpu is bound to '$gpu_driver', not vfio-pci. Bind it yourself first; this script never unbinds host drivers."
[[ $audio_driver == vfio-pci ]] ||
    die "$audio is bound to '$audio_driver', not vfio-pci. The whole IOMMU group must be released before passthrough."

gpu_group=$(readlink -f "/sys/bus/pci/devices/$gpu/iommu_group")
audio_group=$(readlink -f "/sys/bus/pci/devices/$audio/iommu_group")
[[ $gpu_group == "$audio_group" ]] ||
    die "$gpu and $audio are in different IOMMU groups; adjust the topology assumptions first."

note "preflight: $gpu -> vfio-pci (group $(basename "$gpu_group")), $audio -> vfio-pci"

# -------------------------------------------------------------------- build
note "building kernel and root task (profile=$profile) ..."
nix_args=(
    --argstr profile "$profile"
    --arg modules "[ $module ]"
    --no-out-link
)
kernel=$(nix-build "$repo_root/nix" -A sel4.out "${nix_args[@]}")
root=$(nix-build "$repo_root/nix" -A root-task.package "${nix_args[@]}")

[[ -f $kernel/bin/kernel-multiboot.elf ]] || die "kernel image missing in $kernel"
[[ -f $root/bin/task.elf ]] || die "root task image missing in $root"
note "kernel: $kernel"
note "root:   $root"

# -------------------------------------------------------------------- qemu
qemu=${QEMU:-}
if [[ -z $qemu ]]; then
    qemu=$(command -v qemu-system-x86_64 || true)
fi
if [[ -z $qemu ]]; then
    qemu=$(nix-shell "$repo_root/shell.nix" --run 'command -v qemu-system-x86_64')
fi
[[ -x $qemu ]] || die "qemu-system-x86_64 not found; set QEMU=<path>"

run_dir=$out_dir/nvidia-vfio-$(date +%Y%m%d-%H%M%S)
mkdir -p "$run_dir"

command=(
    "$qemu"
    -machine q35,kernel-irqchip=split
    -accel kvm
    -cpu host
    -m "$mem"
    -smp "$cpus"
    -nodefaults
    -no-user-config
    -nic none
    -display none
    -monitor none
    -qmp "unix:$run_dir/qmp.sock,server=on,wait=off"
    -global hpet.msi=on
    -device intel-iommu,intremap=on,caching-mode=on,device-iotlb=off,aw-bits=48
    -device "vfio-pci,host=$gpu,bus=pcie.0,addr=0x8,rombar=0"
    -serial stdio
    -no-reboot
    -kernel "$kernel/bin/kernel-multiboot.elf"
    -initrd "$root/bin/task.elf"
    -append "console_port=0x3f8 debug_port=0x3f8"
)

printf '%s\n' "${command[@]}" >"$run_dir/command.txt"
note "log: $run_dir/serial.log"
note "audio $audio stays with vfio-pci on the host and is not passed to the guest"

if [[ $dry_run == 1 ]]; then
    printf '%q ' "${command[@]}"
    printf '\n'
    exit 0
fi

# QEMU needs the VFIO group; escalate only the emulator, never host bind state.
launcher=()
if [[ $EUID -ne 0 ]]; then
    if [[ -n ${PRIV:-} ]]; then
        read -r -a launcher <<<"$PRIV"
    elif command -v doas >/dev/null; then
        launcher=(doas)
    elif command -v sudo >/dev/null; then
        launcher=(sudo)
    else
        die "not root and neither doas nor sudo found; run as root or set PRIV=<elevation command>"
    fi
    note "elevating QEMU with: ${launcher[*]}"
fi

set -o pipefail
"${launcher[@]}" "${command[@]}" 2>&1 | tee "$run_dir/serial.log"

note
note "expected markers:"
note "  NVIDIA_GSP_BOOT_BEGIN / LAYOUT / STAGE"
note "  NVIDIA_GSP_HANDOFF_BEGIN / RISCV_ACTIVE"
note "  NVIDIA_GSP_INIT_DONE / STATIC_INFO / READY"
note "on failure look for NVIDIA_GSP_FAILED, DEVMGR_MMIO_FAIL,"
note "DEVICE_WINDOW_FAIL, DEVICE_BANK_EXHAUSTED, VTD_FAULT"
note "Ctrl-C kills QEMU; it is not a graceful driver STOP/unload."
