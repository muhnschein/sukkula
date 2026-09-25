/* SPDX-License-Identifier: GPL-3.0-or-later */
#ifndef SUKKULA_TLS_RESERVE_H
#define SUKKULA_TLS_RESERVE_H

#ifdef __cplusplus
extern "C" {
#endif

/* Non-zero when the executable's thread-locals start with the array that
 * keeps bionic's TLS slots out of them (src/tls_reserve.c). Hidden: only
 * main() is exported. */
__attribute__((visibility("hidden"))) int sukkula_tls_reserved(void);

#ifdef __cplusplus
}
#endif

#endif
