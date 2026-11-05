// SPDX-License-Identifier: GPL-2.0-only
// Explicit generic process/thread/VM clients; no raw target TCB or VSpace caps.
#define RAM_BASE 0x40000000UL
#define RAM_SIZE (32UL * 1024 * 1024)
#define BLOCK_SIZE (2UL * 1024 * 1024)
#define TASK_LIMIT 0x10000000UL
#define RAM_CEILING                                                                                \
    (HORN_VM_PROTECTION_READ | HORN_VM_PROTECTION_WRITE | HORN_VM_PROTECTION_EXECUTE)
#define RAM_PROTECTION (HORN_VM_PROTECTION_READ | HORN_VM_PROTECTION_WRITE)
#define MAX_SPACES 8
#define MAX_USER_THREADS 64
static unsigned free_first, free_end, slots_used[256];
static unsigned ram_memory, ram_region, user_context, budget_process;
static unsigned long next_space = 1, next_thread = 1, selected_thread;

struct guest_space {
    unsigned long id;
    unsigned process, space, regions[128];
};

struct guest_thread {
    unsigned long id, space;
    unsigned control;
};
static struct guest_space spaces[MAX_SPACES], kernel_space;
static struct guest_thread threads[MAX_USER_THREADS];

static int available_slots(unsigned count)
{
    for (unsigned s = free_first; s < free_end && s < 256; s++) {
        if (!slots_used[s] && --count == 0) {
            return 1;
        }
    }
    return 0;
}

static unsigned retain(horn_cap_t c)
{
    if (c.slot != RECV || c.mode != HORN_CAP_MOVE) {
        die();
    }
    for (unsigned s = free_first; s < free_end && s < 256; s++) {
        if (!slots_used[s]) {
            if (seL4_CNode_Move(SELF, s, root_depth, SELF, RECV, root_depth)) {
                die();
            }
            slots_used[s] = 1;
            return s;
        }
    }
    die();
    return 0;
}

static void drop(unsigned s)
{
    if (s < free_first || s >= free_end || !slots_used[s]) {
        die();
    }
    if (seL4_CNode_Delete(SELF, s, root_depth)) {
        die();
    }
    slots_used[s] = 0;
}

static struct guest_space *space_by_id(unsigned long id)
{
    if (!id) {
        return &kernel_space;
    }
    for (unsigned i = 0; i < MAX_SPACES; i++) {
        if (spaces[i].id == id) {
            return &spaces[i];
        }
    }
    die();
    return 0;
}

static struct guest_thread *thread_by_id(unsigned long id)
{
    for (unsigned i = 0; i < MAX_USER_THREADS; i++) {
        if (id && threads[i].id == id) {
            return &threads[i];
        }
    }
    return 0;
}

static unsigned empty_process(unsigned long address, unsigned long length)
{
    horn_ProcessEmpty_t q = {0};
    q.launch.name.len = 5;
    memcpy(q.launch.name.data, "guest", 5);
    q.launch.ram_limit = 2 * 1024 * 1024;
    q.address = address;
    q.length = length;
    horn_process_factory_spawn_empty_result_t r;
    if (!available_slots(1)) {
        return 0;
    }
    horn_status_t status =
        horn_process_factory_spawn_empty(&transport, 0, cap(initial.factory), q, &r);
    if (status != HORN_OK) {
        die();
    }
    if (r.tag != HORN_RESULT_OK) {
        if (r.value.err == HORN_PROCESS_ERROR_EXHAUSTED ||
            r.value.err == HORN_PROCESS_ERROR_QUOTA_EXCEEDED) {
            return 0;
        }
        trace_error(__func__, status, r.tag, r.value.err);
        die();
    }
    return retain(r.value.ok);
}

static unsigned reserve_region(unsigned space, unsigned long address, unsigned long length)
{
    horn_VmRegionRequest_t q = {.address = address,
                                .length = length,
                                .alignment = 4096,
                                .placement = HORN_VM_PLACEMENT_FIXED,
                                .ceiling = RAM_CEILING};
    horn_vm_address_space_reserve_result_t r;
    CHECK(horn_vm_address_space_reserve(&transport, 0, cap(space), q, &r), r);
    return retain(r.value.ok);
}

static int apply(unsigned region, horn_VmregionApplyArgs_t *args)
{
    horn_vm_region_apply_result_t r;
    horn_status_t s =
        horn_vm_region_apply(&transport, 0, cap(region), args->memory, args->updates, &r);
    if (s != HORN_OK) {
        die();
    }
    if (r.tag != HORN_RESULT_OK) {
        if (r.value.err.error == HORN_VM_ERROR_QUARANTINED) {
            die();
        }
        return -12;
    }
    return 0;
}

static unsigned mapping_region(struct guest_space *s, unsigned long va, int create)
{
    unsigned long base = s == &kernel_space ? RAM_BASE : 0;
    unsigned long limit = s == &kernel_space ? 0x44000000UL : TASK_LIMIT;
    if (va < base || va >= limit) {
        return 0;
    }
    unsigned i = (va - base) / BLOCK_SIZE;
    if (s == &kernel_space && va < RAM_BASE + RAM_SIZE) {
        return ram_region;
    }
    if (!s->regions[i] && create) {
        s->regions[i] = reserve_region(s->space, va & ~(BLOCK_SIZE - 1), BLOCK_SIZE);
    }
    return s->regions[i];
}

int horn_map(unsigned long id, unsigned long va, unsigned long pfn, unsigned int rights)
{
    if (pfn >= RAM_SIZE / 4096 || va % 4096 ||
        (rights & (HORN_VM_PROTECTION_WRITE | HORN_VM_PROTECTION_EXECUTE)) ==
            (HORN_VM_PROTECTION_WRITE | HORN_VM_PROTECTION_EXECUTE)) {
        return -22;
    }
    struct guest_space *s = space_by_id(id);
    unsigned region = mapping_region(s, va, 1);
    if (!region) {
        return -22;
    }
    unsigned long offset = (region == ram_region) ? va - RAM_BASE : va & (BLOCK_SIZE - 1);
    horn_VmregionApplyArgs_t q = {0};
    q.memory.present = 1;
    q.memory.value = cap(ram_memory);
    q.updates.len = 3;
    q.updates.data[0] =
        (horn_VmUpdate_t){.tag = HORN_VM_UPDATE_UNMAP, .value.unmap = {offset, 4096}};
    q.updates.data[1] =
        (horn_VmUpdate_t){.tag = HORN_VM_UPDATE_PREPARE, .value.prepare = {offset, 4096}};
    q.updates.data[2] = (horn_VmUpdate_t){.tag = HORN_VM_UPDATE_MAP,
                                          .value.map = {offset, 4096, pfn * 4096, rights}};
    return apply(region, &q);
}

int horn_unmap(unsigned long id, unsigned long va, unsigned long length)
{
    if (va % 4096 || length % 4096 || va + length < va) {
        return -22;
    }
    struct guest_space *s = space_by_id(id);
    for (unsigned long end = va + length; va < end;) {
        unsigned region = mapping_region(s, va, 0);
        unsigned long size = BLOCK_SIZE - (va & (BLOCK_SIZE - 1));
        if (size > end - va) {
            size = end - va;
        }
        if (region) {
            unsigned long offset = region == ram_region ? va - RAM_BASE : va & (BLOCK_SIZE - 1);
            horn_VmregionApplyArgs_t q = {0};
            q.updates.len = 1;
            q.updates.data[0] =
                (horn_VmUpdate_t){.tag = HORN_VM_UPDATE_UNMAP, .value.unmap = {offset, size}};
            if (apply(region, &q)) {
                die();
            }
        }
        va += size;
    }
    return 0;
}

unsigned long horn_space_create(void)
{
    unsigned i;
    for (i = 0; i < MAX_SPACES && spaces[i].id; i++)
        ;
    if (i == MAX_SPACES) {
        return 0;
    }
    if (!available_slots(2)) {
        return 0;
    }
    unsigned process = empty_process(0, TASK_LIMIT);
    if (!process) {
        return 0;
    }
    horn_process_control_address_space_result_t r;
    CHECK(horn_process_control_address_space(&transport, 0, cap(process), &r), r);
    spaces[i] =
        (struct guest_space){.id = next_space++, .process = process, .space = retain(r.value.ok)};
    if (!spaces[i].id) {
        die();
    }
    return spaces[i].id;
}

void horn_thread_destroy(unsigned long id)
{
    struct guest_thread *t = thread_by_id(id);
    if (!t) {
        return;
    }
    horn_thread_control_destroy_result_t r;
    CHECK(horn_thread_control_destroy(&transport, 0, cap(t->control), &r), r);
    drop(t->control);
    memset(t, 0, sizeof(*t));
    if (selected_thread == id) {
        selected_thread = 0;
        fp_last_valid = 0;
    }
}

void horn_space_destroy(unsigned long id)
{
    struct guest_space *s = space_by_id(id);
    if (!id) {
        die();
    }
    for (unsigned i = 0; i < MAX_USER_THREADS; i++) {
        if (threads[i].space == id) {
            horn_thread_destroy(threads[i].id);
        }
    }
    horn_process_control_terminate_result_t r;
    CHECK(horn_process_control_terminate(&transport, 0, cap(s->process), &r), r);
    for (unsigned i = 0; i < 128; i++) {
        if (s->regions[i]) {
            drop(s->regions[i]);
        }
    }
    drop(s->space);
    horn_process_control_close_result_t c;
    CHECK(horn_process_control_close(&transport, 0, cap(s->process), &c), c);
    drop(s->process);
    memset(s, 0, sizeof(*s));
}

int horn_thread_prepare(unsigned long space, unsigned long *id)
{
    struct guest_thread *t = thread_by_id(*id);
    if (t && t->space != space) {
        horn_thread_destroy(*id);
        t = 0;
    }
    if (!t) {
        unsigned i;
        for (i = 0; i < MAX_USER_THREADS && threads[i].id; i++)
            ;
        if (i == MAX_USER_THREADS || !available_slots(1)) {
            return -12;
        }
        horn_ThreadCreate_t q = {.abi = HORN_THREAD_ABI_TRAPPED_SYSCALLS,
                                 .allocate_ipc_buffer = false};
        q.registers.ip = 4096;
        q.registers.flags = 0x202;
        horn_process_threads_create_thread_result_t r;
        horn_status_t status = horn_process_threads_create_thread(
            &transport, 0, cap(space_by_id(space)->process), q, &r);
        if (status != HORN_OK) {
            die();
        }
        if (r.tag != HORN_RESULT_OK) {
            if (r.value.err == HORN_PROCESS_ERROR_EXHAUSTED ||
                r.value.err == HORN_PROCESS_ERROR_QUOTA_EXCEEDED) {
                return -12;
            }
            trace_error(__func__, status, r.tag, r.value.err);
            die();
        }
        t = &threads[i];
        *t = (struct guest_thread){
            .id = next_thread++, .space = space, .control = retain(r.value.ok)};
        if (!t->id) {
            die();
        }
        *id = t->id;
        horn_thread_control_watch_result_t w;
        CHECK(
            horn_thread_control_watch(&transport, 0, cap(t->control), cap(initial.user_signal), &w),
            w);
    }
    return 0;
}

int horn_thread_select(unsigned long space, unsigned long *id)
{
    int error = horn_thread_prepare(space, id);
    if (error) {
        return error;
    }
    if (selected_thread != *id) {
        selected_thread = *id;
        fp_last_valid = 0;
    }
    return 0;
}

static unsigned current_thread(void)
{
    struct guest_thread *t = thread_by_id(selected_thread);
    if (!t) {
        die();
    }
    return t->control;
}

static void generic_init(unsigned first, unsigned end)
{
    free_first = first;
    free_end = end;
    if (end > 256) {
        die();
    }
    kernel_space.space = initial.address_space;
    horn_vm_factory_allocate_result_t m;
    CHECK(horn_vm_factory_allocate(&transport, 0, cap(initial.vm), RAM_SIZE,
                                   HORN_VM_BACKING_RESIDENT, RAM_CEILING, &m),
          m);
    ram_memory = retain(m.value.ok);
    ram_region = reserve_region(initial.address_space, RAM_BASE, RAM_SIZE);
    horn_VmregionApplyArgs_t q = {0};
    q.memory.present = 1;
    q.memory.value = cap(ram_memory);
    q.updates.len = 2;
    q.updates.data[0] =
        (horn_VmUpdate_t){.tag = HORN_VM_UPDATE_PREPARE, .value.prepare = {0, RAM_SIZE}};
    q.updates.data[1] =
        (horn_VmUpdate_t){.tag = HORN_VM_UPDATE_MAP, .value.map = {0, RAM_SIZE, 0, RAM_PROTECTION}};
    if (apply(ram_region, &q)) {
        die();
    }
    budget_process = empty_process(4096, 4096);
    if (!budget_process) {
        die();
    }
    horn_process_threads_execution_context_result_t c;
    CHECK(horn_process_threads_execution_context(&transport, 0, cap(budget_process), &c), c);
    user_context = retain(c.value.ok);
    // Query generic FP metadata on a stopped TCB, not a Linux execution domain.
    horn_ThreadCreate_t request = {.abi = HORN_THREAD_ABI_TRAPPED_SYSCALLS};
    request.registers.ip = 4096;
    request.registers.flags = 0x202;
    horn_process_threads_create_thread_result_t t;
    CHECK(horn_process_threads_create_thread(&transport, 0, cap(budget_process), request, &t), t);
    unsigned probe = retain(t.value.ok);
    horn_thread_control_fp_info_result_t info;
    CHECK(horn_thread_control_fp_info(&transport, 0, cap(probe), &info), info);
    fp_info = info.value.ok;
    horn_thread_control_destroy_result_t d;
    CHECK(horn_thread_control_destroy(&transport, 0, cap(probe), &d), d);
    drop(probe);
    if (fp_info.bytes < 576 || fp_info.bytes > sizeof fp_last || (fp_info.features & 7) != 7) {
        die();
    }
}
