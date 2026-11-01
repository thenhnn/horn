/* Only includes: all ABI declarations come from unmodified open-gpu 570.144. */
#include <nvtypes.h>
#include <nvstatus.h>
#include <nvmisc.h>
#include <nvos.h>
#include <class/cl0000.h>
#include <class/cl0040.h>
#include <class/cl0073.h>
#include <class/cl0080.h>
#include <class/cl2080.h>
#include <class/cl90f1.h>
/* Legacy user-register byte pointers, as used by nouveau's NV507C accessor. */
#include <class/cl917c.h>
#include <swref/published/disp/v03_00/dev_disp.h>
#include <swref/published/maxwell/gm107/dev_bus.h>
#include <class/clc0b5.h>
#include <class/clc0b5sw.h>
#include <class/clc56f.h>
#include <class/clc6b5.h>
#include <class/clc7b5.h>
#include <class/clc770.h>
#include <class/clc77d.h>
#include <class/clc67e.h>
#include <class/clc57e.h>
#include <class/clc37e.h>
#include <class/clc67a.h>
#include <alloc/alloc_channel.h>
#include <ctrl/ctrl0073/ctrl0073system.h>
#include <ctrl/ctrl0073/ctrl0073specific.h>
#include <ctrl/ctrl0073/ctrl0073dfp.h>
#include <ctrl/ctrl0073/ctrl0073dp.h>
#include <ctrl/ctrl0080/ctrl0080dma.h>
#include <ctrl/ctrl2080/ctrl2080internal.h>
#include <ctrl/ctrl2080/ctrl2080fifo.h>
#include <ctrl/ctrl2080/ctrl2080ce.h>
#include <ctrl/ctrla06f/ctrla06fgpfifo.h>
#define RPC_STRUCTURES
#include <g_rpc-structures.h>

/* Evaluate NVIDIA's function-like/range macros with clang, not handwritten
 * Rust shifts. This adds no ABI declarations or copied field definitions. */
enum {
    HORN_PRAMIN_WINDOW = NV_PBUS_BAR0_WINDOW,
    HORN_PRAMIN_TARGET_MASK = DRF_SHIFTMASK(NV_PBUS_BAR0_WINDOW_TARGET),
    HORN_PRAMIN_BASE_MASK = DRF_MASK(NV_PBUS_BAR0_WINDOW_BASE),
    HORN_PRAMIN_BASE_SHIFT = DRF_SHIFT(NV_PBUS_BAR0_WINDOW_BASE),
    HORN_NOTIFIER_ENABLED = DRF_DEF(C77D, _SET_NOTIFIER_CONTROL, _MODE, _WRITE) |
                            DRF_DEF(C77D, _SET_NOTIFIER_CONTROL, _NOTIFY, _ENABLE),
    HORN_NOTIFIER_STATUS_SHIFT = DRF_SHIFT(NV_DISP_NOTIFIER__0_STATUS),
    HORN_NOTIFIER_STATUS_MASK = DRF_MASK(NV_DISP_NOTIFIER__0_STATUS),
    HORN_VASPACE_EXTERNALLY_OWNED = NV_VASPACE_ALLOCATION_FLAGS_IS_EXTERNALLY_OWNED,
    HORN_CHANNEL_PRIVILEGED = DRF_DEF(OS04, _FLAGS, _PRIVILEGED_CHANNEL, _TRUE),
    HORN_CHANNEL_USERD_PAGE_FIXED = DRF_DEF(OS04, _FLAGS, _CHANNEL_USERD_INDEX_PAGE_FIXED, _TRUE),
    HORN_CHANNEL_USERD_SHIFT = DRF_SHIFT(NVOS04_FLAGS_CHANNEL_USERD_INDEX_VALUE),
    HORN_CHANNEL_USERD_MASK = DRF_MASK(NVOS04_FLAGS_CHANNEL_USERD_INDEX_VALUE),
    HORN_DISP_COUNT_SHIFT = DRF_SHIFT(NVC77D_DMA_METHOD_COUNT),
    HORN_DISP_COUNT_MASK = DRF_MASK(NVC77D_DMA_METHOD_COUNT),
    HORN_DISP_METHOD_SHIFT = DRF_SHIFT(NVC77D_DMA_METHOD_OFFSET),
    HORN_DISP_METHOD_MASK = DRF_SHIFTMASK(NVC77D_DMA_METHOD_OFFSET),
    HORN_DISP_GET_MASK = DRF_SHIFTMASK(NV917C_GET_PTR),
    HORN_DISP_GET_SHIFT = DRF_SHIFT(NV917C_GET_PTR),
    HORN_DISP_PUT_MASK = DRF_MASK(NV917C_PUT_PTR),
    HORN_DISP_PUT_SHIFT = DRF_SHIFT(NV917C_PUT_PTR),
    HORN_DISP_UPDATE = DRF_DEF(C77D, _UPDATE, _SPECIAL_HANDLING, _NONE) |
                       DRF_DEF(C77D, _UPDATE, _INHIBIT_INTERRUPTS, _FALSE) |
                       DRF_DEF(C77D, _UPDATE, _RELEASE_ELV, _TRUE),
    HORN_DISP_NOTIFIER_DISABLED = DRF_DEF(C77D, _SET_NOTIFIER_CONTROL, _NOTIFY, _DISABLE),
};
