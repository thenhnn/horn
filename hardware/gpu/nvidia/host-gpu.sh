#!/usr/bin/env bash
# Host-side GPU ownership switch between the proprietary NVIDIA driver and
# vfio-pci, so the GPU can be passed through to Horn.
#
# THIS SCRIPT MUTATES HOST DRIVER STATE. It is an explicitly authorized
# operator action: run it yourself, read the plan it prints, and confirm.
# It never performs a PCI reset, never uses `rmmod -f`, never touches ACS or
# IOMMU kernel parameters.
#
# Usage:
#   host-gpu.sh status            # read-only report
#   host-gpu.sh release [--yes]   # stop users of the GPU, unload nvidia,
#                                 # bind GPU + audio to vfio-pci
#   host-gpu.sh restore [--yes]   # unbind vfio-pci, reload nvidia,
#                                 # restart the display manager
#   any of the above with --dry-run
#
# Environment:
#   GPU=0000:01:00.0     GPU function
#   AUDIO=0000:01:00.1   sibling audio function (same IOMMU group)
#   PRIV=<cmd>           elevation command (default: doas, then sudo)
set -euo pipefail

gpu=${GPU:-0000:01:00.0}
audio=${AUDIO:-0000:01:00.1}
dry_run=0
assume_yes=0
command_name=

usage() {
    sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

note() {
    printf '%s\n' "$*" >&2
}

run() {
    if [[ $dry_run == 1 ]]; then
        printf '+ %s\n' "$*" >&2
        return 0
    fi
    "$@"
}

# Writes go through `tee` so the exact sysfs operation is visible in the log.
write() {
    local value=$1 path=$2
    if [[ $dry_run == 1 ]]; then
        printf '+ printf %q > %s\n' "$value" "$path" >&2
        return 0
    fi
    printf '%s' "$value" >"$path"
}

original_args=("$@")
while (($#)); do
    case $1 in
    status | release | restore)
        command_name=$1
        shift
        ;;
    --yes | -y)
        assume_yes=1
        shift
        ;;
    --dry-run | -n)
        dry_run=1
        shift
        ;;
    -h | --help)
        usage
        exit 0
        ;;
    *)
        usage >&2
        exit 2
        ;;
    esac
done
[[ -n $command_name ]] || {
    usage >&2
    exit 2
}

if [[ $EUID -ne 0 && $command_name != status && $dry_run == 0 ]]; then
    if [[ -n ${PRIV:-} ]]; then
        exec $PRIV -- "${BASH_SOURCE[0]}" "${original_args[@]}"
    elif command -v doas >/dev/null; then
        exec doas -- "${BASH_SOURCE[0]}" "${original_args[@]}"
    elif command -v sudo >/dev/null; then
        exec sudo -- "${BASH_SOURCE[0]}" "${original_args[@]}"
    fi
    die "not root and neither doas nor sudo found"
fi

pci_driver() {
    local path=/sys/bus/pci/devices/$1/driver
    if [[ -L $path ]]; then
        basename "$(readlink -f "$path")"
    else
        printf 'none\n'
    fi
}

module_loaded() {
    [[ -d /sys/module/$1 ]]
}

# Processes holding an NVIDIA character device. /dev/dri is not scanned: it may
# belong to an unrelated integrated GPU.
gpu_users() {
    local fd target pid found=1
    for pid in /proc/[0-9]*; do
        [[ -d $pid/fd ]] || continue
        for fd in "$pid"/fd/*; do
            target=$(readlink -f "$fd" 2>/dev/null) || continue
            [[ $target == /dev/nvidia* ]] || continue
            printf '%s %s\n' "${pid#/proc/}" "$target"
            found=0
        done
    done
    return $found
}

systemctl_present() {
    command -v systemctl >/dev/null && [[ -d /run/systemd/system ]]
}

unit_active() {
    systemctl_present && systemctl is-active --quiet "$1"
}

status() {
    local device
    printf 'GPU  %s -> %s\n' "$gpu" "$(pci_driver "$gpu")"
    printf 'AUDIO %s -> %s\n' "$audio" "$(pci_driver "$audio")"
    if [[ -e /sys/bus/pci/devices/$gpu/iommu_group ]]; then
        printf 'IOMMU group %s\n' "$(basename "$(readlink -f "/sys/bus/pci/devices/$gpu/iommu_group")")"
        for device in "/sys/bus/pci/devices/$gpu/iommu_group/devices/"*; do
            printf '  %s -> %s\n' "$(basename "$device")" "$(pci_driver "$(basename "$device")")"
        done
    fi
    if [[ -e /sys/bus/pci/devices/$gpu/reset_method ]]; then
        printf 'reset_method: %s\n' "$(cat "/sys/bus/pci/devices/$gpu/reset_method")"
    fi
    printf 'modules:'
    for device in nvidia nvidia_modeset nvidia_drm nvidia_uvm nvidia_peermem vfio_pci; do
        module_loaded "$device" && printf ' %s' "$device"
    done
    printf '\n'
    if systemctl_present; then
        local dm=inactive
        dm=$(systemctl is-active display-manager.service 2>/dev/null || true)
        printf 'display-manager: %s\n' "${dm:-inactive}"
    fi
    if users=$(gpu_users); then
        printf 'GPU users:\n%s\n' "$users"
    else
        printf 'GPU users: none\n'
    fi
}

confirm() {
    local answer
    [[ $assume_yes == 1 ]] && return 0
    [[ $dry_run == 1 ]] && return 0
    printf '%s [type "yes"]: ' "$1" >&2
    read -r answer
    [[ $answer == yes ]] || die "not confirmed"
}

release() {
    local module
    note "plan: stop GPU users and the display manager, unload the NVIDIA"
    note "      modules, bind $gpu and $audio to vfio-pci."
    note "      No PCI reset is issued. The guest run needs both functions."
    confirm "Proceed with the release to vfio-pci?"

    if systemctl_present; then
        if unit_active nvidia-persistenced.service; then
            run systemctl stop nvidia-persistenced.service
        fi
        if unit_active display-manager.service; then
            run systemctl stop display-manager.service
        fi
    fi

    if [[ $dry_run == 0 ]] && users=$(gpu_users); then
        note "these processes still hold /dev/nvidia*:"
        printf '%s\n' "$users" >&2
        die "stop them first; the script does not kill processes"
    fi

    # Fixed order: dependencies before the core module. Never `rmmod -f`.
    for module in nvidia_peermem nvidia_drm nvidia_modeset nvidia_uvm nvidia; do
        if module_loaded "$module"; then
            run modprobe -r "$module"
        fi
    done
    run modprobe vfio-pci

    local device
    for device in "$gpu" "$audio"; do
        [[ -e /sys/bus/pci/devices/$device ]] || die "missing PCI device $device"
        write 'vfio-pci' "/sys/bus/pci/devices/$device/driver_override"
        if [[ -L /sys/bus/pci/devices/$device/driver ]]; then
            write "$device" "/sys/bus/pci/devices/$device/driver/unbind"
        fi
        write "$device" /sys/bus/pci/drivers_probe
    done

    if [[ $dry_run == 0 ]]; then
        for device in "$gpu" "$audio"; do
            [[ $(pci_driver "$device") == vfio-pci ]] ||
                die "$device did not bind to vfio-pci; check dmesg/journalctl -k"
        done
        note "released: $gpu and $audio are on vfio-pci"
    fi
}

restore() {
    local module device
    note "plan: unbind $gpu and $audio from vfio-pci, clear driver_override,"
    note "      reload the NVIDIA modules and restart the display manager."
    note "      No PCI reset is issued. If this fails, reboot instead of"
    note "      forcing resets."
    confirm "Proceed with restoring the NVIDIA driver?"

    for device in "$gpu" "$audio"; do
        [[ -e /sys/bus/pci/devices/$device ]] || die "missing PCI device $device"
        if [[ $(pci_driver "$device") == vfio-pci ]]; then
            write "$device" /sys/bus/pci/drivers/vfio-pci/unbind
        fi
        write '' "/sys/bus/pci/devices/$device/driver_override"
    done

    for module in nvidia nvidia_modeset nvidia_drm; do
        module_loaded "$module" || run modprobe "$module"
    done

    for device in "$gpu" "$audio"; do
        write "$device" /sys/bus/pci/drivers_probe
    done

    if [[ $dry_run == 0 ]]; then
        for device in "$gpu" "$audio"; do
            note "$device -> $(pci_driver "$device")"
        done
        if [[ $(pci_driver "$gpu") != nvidia ]]; then
            note "diagnostics: journalctl -k -n 80"
            die "$gpu did not return to the nvidia driver; reboot rather than forcing a reset"
        fi
        if systemctl_present && systemctl cat display-manager.service >/dev/null 2>&1; then
            run systemctl start display-manager.service
        fi
    fi
}

case $command_name in
status) status ;;
release) release ;;
restore) restore ;;
esac
