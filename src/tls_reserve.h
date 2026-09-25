/* SPDX-License-Identifier: GPL-3.0-or-later */
#ifndef SUKKULA_TLS_RESERVE_H
#define SUKKULA_TLS_RESERVE_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The size of the executable's only thread-local, the array at tp+16
 * that the graphics stack's thread-locals and bionic's TLS slots take
 * (src/tls_reserve.c). ci/check-elf.sh --tls-reserve checks the same
 * number. */
#define SUKKULA_TLS_RESERVE 4096

/* The last bytes of the array that must still be zero when the engine
 * starts; anything the graphics stack wrote in them may have run on
 * past the array. */
#define SUKKULA_TLS_RESERVE_MARGIN 256

/* Non-zero when the executable's thread-locals are the array, at tp+16,
 * or when the binary was dlopen()ed by the booster and the thread pointer
 * is not its to check. Hidden: only main() is exported. */
__attribute__((visibility("hidden"))) int sukkula_tls_reserved(void);

/* How much of the array the calling thread has seen written, counted from
 * its start: the offset one past the last byte that is not zero. 0 when
 * dlopen()ed by the booster. */
__attribute__((visibility("hidden"))) size_t sukkula_tls_reserve_used(void);

#ifdef __cplusplus
}
#endif

#endif
