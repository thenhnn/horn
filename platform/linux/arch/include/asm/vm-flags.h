/*
 * Copyright (C) 2004 Jeff Dike (jdike@addtoit.com)
 * Copyright 2003 PathScale, Inc.
 * Licensed under the GPL
 */

#ifndef __VM_FLAGS_X86_H
#define __VM_FLAGS_X86_H

/* Horn is x86-64 only. Anonymous data and stacks must default to NX;
 * UML's executable-data fallback conflicts with backing-wide W^X. */
#define VM_DATA_DEFAULT_FLAGS VM_DATA_FLAGS_NON_EXEC
#define VM_STACK_DEFAULT_FLAGS (VM_GROWSDOWN | VM_DATA_FLAGS_NON_EXEC)
#endif
