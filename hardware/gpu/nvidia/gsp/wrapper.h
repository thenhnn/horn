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
#include <class/clc0b5.h>
#include <class/clc0b5sw.h>
#include <class/clc56f.h>
#include <class/clc6b5.h>
#include <class/clc7b5.h>
#include <class/clc770.h>
#include <class/clc77d.h>
#include <class/clc67e.h>
#include <class/clc67a.h>
#include <alloc/alloc_channel.h>
#include <ctrl/ctrl0073/ctrl0073system.h>
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
    HORN_VASPACE_EXTERNALLY_OWNED = NV_VASPACE_ALLOCATION_FLAGS_IS_EXTERNALLY_OWNED,
    HORN_CHANNEL_PRIVILEGED = DRF_DEF(OS04, _FLAGS, _PRIVILEGED_CHANNEL, _TRUE),
    HORN_CHANNEL_USERD_PAGE_FIXED = DRF_DEF(OS04, _FLAGS, _CHANNEL_USERD_INDEX_PAGE_FIXED, _TRUE),
    HORN_CHANNEL_USERD_SHIFT = DRF_SHIFT(NVOS04_FLAGS_CHANNEL_USERD_INDEX_VALUE),
    HORN_CHANNEL_USERD_MASK = DRF_MASK(NVOS04_FLAGS_CHANNEL_USERD_INDEX_VALUE),
};
