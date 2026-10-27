/* SPDX-License-Identifier: MIT */
/* Horn's resident-memory mimalloc primitive backend. */
#include "mimalloc/prim.h"
#include <errno.h>

#define MI_PRIM_HAS_PROCESS_ATTACH 1

extern void* horn_mimalloc_alloc(size_t size, size_t alignment);
extern int horn_mimalloc_free(void* address, size_t size);
extern size_t horn_mimalloc_limit(void);
extern void horn_mimalloc_message(const char* message);
extern void horn_mimalloc_yield(void);
extern void horn_mimalloc_abort(void);

void _mi_prim_mem_init(mi_os_mem_config_t* config) {
  config->page_size = 4096;
  config->alloc_granularity = 4096;
  config->large_page_size = 0;
  config->physical_memory_in_kib = horn_mimalloc_limit() / 1024;
  config->virtual_address_bits = 32;
  config->has_overcommit = false;
  config->has_partial_free = false;
  config->has_virtual_reserve = false;
  config->has_transparent_huge_pages = false;
}
int _mi_prim_alloc(void* hint, size_t size, size_t alignment, bool commit,
                   bool allow_large, bool* is_large, bool* is_zero, void** address) {
  (void)hint; (void)allow_large;
  *address = NULL; *is_large = false; *is_zero = false;
  if (!commit) return ENOTSUP;
  *address = horn_mimalloc_alloc(size, alignment);
  if (*address == NULL) return ENOMEM;
  *is_zero = true;
  return 0;
}
int _mi_prim_free(void* address, size_t size) { return horn_mimalloc_free(address, size); }
int _mi_prim_commit(void* address, size_t size, bool* is_zero) {
  (void)address; (void)size; *is_zero = false;
  return 0; /* All successful allocations are resident and committed. */
}
int _mi_prim_decommit(void* address, size_t size, bool* needs_recommit) {
  (void)address; (void)size; *needs_recommit = false;
  return ENOTSUP;
}
int _mi_prim_reset(void* address, size_t size) { (void)address; (void)size; return ENOTSUP; }
int _mi_prim_reuse(void* address, size_t size) { (void)address; (void)size; return 0; }
int _mi_prim_protect(void* address, size_t size, bool protect) {
  (void)address; (void)size; (void)protect; return ENOTSUP;
}
int _mi_prim_alloc_huge_os_pages(void* hint, size_t size, int node, bool* zero, void** address) {
  (void)hint; (void)size; (void)node; *zero = false; *address = NULL; return ENOTSUP;
}
size_t _mi_prim_numa_node(void) { return 0; }
size_t _mi_prim_numa_node_count(void) { return 1; }
mi_msecs_t _mi_prim_clock_now(void) { return 0; } /* Timed purging is disabled. */
void _mi_prim_process_info(mi_process_info_t* info) { (void)info; }
void _mi_prim_out_stderr(const char* message) { horn_mimalloc_message(message); }
int _mi_prim_getenv(const char* name, char* result, size_t length) {
  (void)name; (void)result; (void)length; return 0;
}
bool _mi_prim_random_buf(void* buffer, size_t length) { (void)buffer; (void)length; return false; }
void _mi_prim_thread_init_auto_done(void) { }
void _mi_prim_thread_done_auto_done(void) { }
void _mi_prim_thread_associate_default_theap(mi_theap_t* theap) { (void)theap; }
bool _mi_prim_thread_is_in_threadpool(void) { return false; }
void _mi_prim_thread_yield(void) { horn_mimalloc_yield(); }
bool _mi_prim_process_is_killed(void) { return false; }

/* The freestanding image has no libc errno or abort implementation. */
static __thread int horn_errno;
int* __errno_location(void) { return &horn_errno; }
void abort(void) { horn_mimalloc_abort(); __builtin_unreachable(); }
