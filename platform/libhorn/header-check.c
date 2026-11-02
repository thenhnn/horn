/* Compile-only ABI check against the generated header. */
#include <stddef.h>
#include <horn/startup.h>

_Static_assert(sizeof(HornStartup) == HORN_STARTUP_HEADER_SIZE, "startup ABI");
_Static_assert(sizeof(HornContextEntry) == HORN_CONTEXT_ENTRY_SIZE, "entry ABI");
_Static_assert(offsetof(HornStartup, cspace_bits) == 72, "radix offset");
_Static_assert(offsetof(HornStartup, free_end) == 88, "range offset");

void horn_header_check(void)
{
    int32_t (*validate)(const uint8_t *, size_t) = horn_startup_validate;
    int32_t (*lookup)(const uint8_t *, size_t, const uint8_t *, size_t,
                      uint64_t *) = horn_startup_lookup;
    (void)validate;
    (void)lookup;
}
