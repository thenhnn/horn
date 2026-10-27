/* SPDX-License-Identifier: MIT */
#include "mimalloc.h"
extern void _mi_auto_process_init(void);
void horn_mimalloc_start(void) {
  mi_option_set(mi_option_arena_reserve, 0);
  mi_option_set(mi_option_purge_delay, -1);
  mi_option_set(mi_option_pagemap_commit, 1);
  _mi_auto_process_init();
}
