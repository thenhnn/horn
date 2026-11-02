use std::{env, path::PathBuf};

fn main() {
    for name in ["NVIDIA_OPEN_GPU_SOURCE", "LIBCLANG_PATH"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let source = PathBuf::from(
        env::var_os("NVIDIA_OPEN_GPU_SOURCE")
            .expect("NVIDIA_OPEN_GPU_SOURCE is required; use nix-shell (pinned open-gpu 570.144)"),
    );
    // No local work/ checkout, network access or copied ABI declarations.
    let mut bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .use_core()
        .ctypes_prefix("core::ffi")
        .derive_default(true)
        .wrap_unsafe_ops(true)
        .generate_comments(false)
        .layout_tests(true)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .allowlist_type("rpc_gsp_rm_(alloc|control)_v03_00")
        .allowlist_type("rpc_post_event_v17_00|NV0005_ALLOC_PARAMETERS|NV2080_CTRL_EVENT_SET_NOTIFICATION_PARAMS|Nv2080(Hotplug|DpIrq)Notification")
        .allowlist_var("NV01_EVENT_(KERNEL_CALLBACK_EX|CLIENT_RM)|NV2080_NOTIFIERS_(HOTPLUG|DP_IRQ)|NV2080_CTRL_(CMD_EVENT_SET_NOTIFICATION|EVENT_SET_NOTIFICATION_ACTION_REPEAT)")
        .allowlist_type("NV(0000_ALLOC_PARAMETERS|0080_ALLOC_PARAMETERS|2080_ALLOC_PARAMETERS|_MEMORY_ALLOCATION_PARAMS|_MEMORY_VIRTUAL_ALLOCATION_PARAMS|_VASPACE_ALLOCATION_PARAMETERS|_MEMORY_DESC_PARAMS|_CHANNEL_ALLOC_PARAMS)")
        .allowlist_type("NV50VAIO_CHANNEL(DMA|PIO)_ALLOCATION_PARAMETERS")
        .allowlist_type("NV0073_CTRL_(SPECIFIC_(GET_CONNECTOR_DATA|OR_GET_INFO)_PARAMS|DFP_ASSIGN_SOR_PARAMS|DP_(CTRL|AUXCH_CTRL)_PARAMS|CMD_DP_(SET_MANUAL_DISPLAYPORT|CONFIG_STREAM|GET_CAPS)_PARAMS)")
        .allowlist_type("NV(0073_CTRL_SYSTEM_GET_(SUPPORTED|ACTIVE|CONNECT_STATE)_PARAMS|2080_CTRL_INTERNAL_DISPLAY_(GET_STATIC_INFO|WRITE_INST_MEM|CHANNEL_PUSHBUFFER)_PARAMS|2080_CTRL_CE_GET_FAULT_METHOD_BUFFER_SIZE_PARAMS|2080_CTRL_FIFO_GET_DEVICE_INFO_TABLE_PARAMS|A06F_CTRL_BIND_PARAMS|A06F_CTRL_GPFIFO_SCHEDULE_PARAMS|C0B5_ALLOCATION_PARAMETERS|0080_CTRL_DMA_SET_PAGE_DIRECTORY_PARAMS)")
        .allowlist_var("(ChannelPBSize_.*|NV50VAIO_CHANNELDMA_ALLOCATION_FLAGS_.*|NV01_ROOT|NV01_DEVICE_0|NV20_SUBDEVICE_0|NV04_DISPLAY_COMMON|AMPERE_CHANNEL_GPFIFO_A|AMPERE_DMA_COPY_.*|FERMI_VASPACE_A)")
        .allowlist_var("NV_ERR_.*|NV_OK|HORN_.*")
        .allowlist_var("NV0073_CTRL_(CMD_.*|SPECIFIC_OR_.*|SYSTEM_GET_ACTIVE_FLAGS_CLIENT_(ENABLE|DISABLE))")
        .allowlist_var("NVC57E_.*|NVC37E_.*|NVC77D_.*|NV_DISP_NOTIFIER.*")
        .allowlist_var("NVC77D_(DMA_DATA_NOP|SET_NOTIFIER_CONTROL|SET_INTERLOCK_FLAGS|SET_WINDOW_INTERLOCK_FLAGS|UPDATE)")
        .allowlist_var("NV917C_(PUT|GET)|NV_UDISP_FE_CHN_ASSY_BASEADR_CORE|NV_DMA_TARGET_NODE_PHYSICAL_PCI_COHERENT")
        .allowlist_var("NV(C770_DISPLAY|C77D_CORE_CHANNEL_DMA|C67E_WINDOW_CHANNEL_DMA|C67A_CURSOR_IMM_CHANNEL_PIO|01_MEMORY_LOCAL_USER|C0B5_ALLOCATION_PARAMETERS_VERSION_.*|C7B5_.*|0073_CTRL_CMD_SYSTEM_GET_SUPPORTED|2080_CTRL_CMD_INTERNAL_DISPLAY_.*|2080_CTRL_CMD_CE_GET_FAULT_METHOD_BUFFER_SIZE|2080_CTRL_CMD_FIFO_GET_DEVICE_INFO_TABLE|A06F_CTRL_CMD_BIND|A06F_CTRL_CMD_GPFIFO_SCHEDULE|0080_CTRL_CMD_DMA_SET_PAGE_DIRECTORY|OS32_TYPE_IMAGE|OS32_ALLOC_FLAGS_.*|_VASPACE_ALLOCATION_FLAGS_.*)")
        // This driver targets AD106 on x86_64. Do not let build-host ABI or
        // seL4's custom target triple change the C wire layout.
        .clang_args(["-x", "c", "-std=c11", "--target=x86_64-unknown-none"]);
    for path in [
        "src/common/sdk/nvidia/inc",
        "src/common/inc",
        "src/nvidia/generated",
        "src/nvidia/inc",
        "src/nvidia/inc/kernel",
    ] {
        bindings = bindings.clang_arg(format!("-I{}", source.join(path).display()));
    }
    bindings
        .generate()
        .expect("generate NVIDIA r570.144 bindings")
        .write_to_file(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("rm_bindings.rs"))
        .expect("write NVIDIA bindings");
}
