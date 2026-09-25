/* SPDX-License-Identifier: GPL-3.0-or-later
 *
 * Bionic's thread-pointer slots, kept clear of the engine's thread-locals.
 *
 * The phone's graphics stack is Android's, run in this glibc process by
 * libhybris, and Android code reaches its per-thread slots straight off the
 * thread pointer: on arm64, TLS_SLOT_APP to TLS_SLOT_ART_THREAD_SELF are
 * the words at tp+16 to tp+63 (bionic's tls_defines.h), and EGL and GL
 * write TLS_SLOT_OPENGL and TLS_SLOT_OPENGL_API there on the GUI thread.
 * Bionic keeps them free by requiring every executable to align its TLS
 * segment to 64. glibc does not: following the ELF ABI, it puts the
 * executable's TLS block right after its 16-byte TCB, at tp+16. The Rust
 * engine is linked into this executable, so that is where its
 * thread-locals were -- tokio's runtime context first, whose RefCell
 * borrow flag the GL driver overwrote before QML started the engine, which
 * then panicked entering its runtime ("RefCell already borrowed"; the test
 * is ci/tls-slots-test.sh).
 *
 * So the first 48 bytes of the executable's TLS segment are this array,
 * which nothing reads: what Android writes into the slots lands here. It
 * is first because this is the first object on the link line
 * (harbour-sukkula.pro lists it first, and the C runtime's objects carry no
 * thread-locals), it is .tdata, which comes before every .tbss, and the
 * segment is aligned to no more than 16 bytes, so the block starts at
 * tp+16 exactly. Aligning the segment to 64 instead would also move the
 * engine past the slots, but glibc gives the 48-byte gap that leaves to the
 * first library whose thread-locals fit, and that library would be
 * overwritten instead.
 *
 * ci/check-elf.sh reads the marker back as the first bytes of the packaged
 * binary's TLS image; sukkula_tls_reserved() checks the address itself at
 * start-up.
 */
#include "tls_reserve.h"

/* 47 characters and the NUL: tp+16 to tp+63. */
static __thread char bionic_slots[48] __attribute__((used, aligned(16)))
    = "sukkula: bionic TLS slots 2..7, tp+16 to tp+63.";

_Static_assert(sizeof bionic_slots == 64 - 16, "tp+16 to tp+63");

int sukkula_tls_reserved(void)
{
#if defined(__aarch64__)
    char *tp;
    __asm__("mrs %0, tpidr_el0" : "=r"(tp));
    return bionic_slots == tp + 16;
#else
    /* The slots are arm64's; the host build only compiles this. */
    return 1;
#endif
}
