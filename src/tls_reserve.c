/* SPDX-License-Identifier: GPL-3.0-or-later
 *
 * The words at the thread pointer that the phone's graphics stack takes,
 * kept clear of every library's thread-locals.
 *
 * The phone's EGL and GL are Android's, run in this glibc process by
 * libhybris, and Android code keeps its per-thread data at fixed offsets
 * from the thread pointer:
 *
 * - bionic's TLS slots, TLS_SLOT_APP to TLS_SLOT_ART_THREAD_SELF, the
 *   words at tp+16 to tp+63 on arm64 (bionic's tls_defines.h), where EGL
 *   and GL keep TLS_SLOT_OPENGL and TLS_SLOT_OPENGL_API;
 * - the thread-locals of every Android library, the Mali driver's among
 *   them. libhybris's linker gives each such module a static TLS offset
 *   counted from 0 and adds it to the thread pointer as it is
 *   (hybris/common/q: linker_tls.cpp, and linker.cpp's TPREL and TLSDESC
 *   relocations), so they start at tp+0 and run as far as those modules
 *   need. Nothing initialises that memory for them either: Android code
 *   reads whatever is there as its own starting state.
 *
 * glibc puts the first TLS block of the process at tp+16. That block is
 * the executable's if the executable has thread-locals, and otherwise the
 * first library's. The first RPM had the engine linked into the
 * executable, and the GL stack overwrote tokio's runtime context there
 * ("RefCell already borrowed"). The second had a 48-byte marker there
 * instead, which Android code read back as its own state, and the process
 * died inside the GL stack.
 *
 * So the executable's only thread-local is this array. It is all zero,
 * which is the starting state bionic would give Android code, and one
 * page long, far more than the slots and the GL stack's thread-locals
 * take. It is .tbss and the executable's only TLS, and the segment is
 * aligned to 16, so the block starts at tp+16 exactly: glibc puts nothing
 * of the process's own before tp+16+4096.
 *
 * Checked by ci/check-elf.sh --tls-reserve 4096, which reads the packaged
 * binary's TLS segment back; at start-up by sukkula_tls_reserved(); before
 * the engine starts by sukkula_tls_reserve_used() (src/bridge.cpp); and
 * under qemu by ci/hybris-tls-test.sh.
 */
#include "tls_reserve.h"

#include <link.h>
#include <sys/auxv.h>

static __thread char reserve[SUKKULA_TLS_RESERVE] __attribute__((used, aligned(16)));

_Static_assert(sizeof reserve >= 64 - 16, "at least bionic's slots, tp+16 to tp+63");

/* The booster dlopen()s this binary. Then the executable's thread-locals
 * are not at the thread pointer, whose words are the booster's and none of
 * this file's business. */
static int is_main_program(void)
{
    extern const char __ehdr_start[] __attribute__((visibility("hidden")));
    const ElfW(Ehdr) *ehdr = (const ElfW(Ehdr) *)(const void *)__ehdr_start;
    return getauxval(AT_PHDR) == (unsigned long)(__ehdr_start + ehdr->e_phoff);
}

int sukkula_tls_reserved(void)
{
#if defined(__aarch64__)
    if (!is_main_program())
        return 1;
    char *tp;
    __asm__("mrs %0, tpidr_el0" : "=r"(tp));
    return reserve == tp + 16;
#else
    /* libhybris and bionic's slots are the phone's; the host build only
     * compiles this. */
    return 1;
#endif
}

size_t sukkula_tls_reserve_used(void)
{
    /* Volatile: nothing here writes the array, and the compiler would
     * otherwise read it as the zeros it starts as. */
    const volatile char *bytes = reserve;
    size_t used = sizeof reserve;

    if (!is_main_program())
        return 0;
    while (used > 0 && bytes[used - 1] == 0)
        used--;
    return used;
}
