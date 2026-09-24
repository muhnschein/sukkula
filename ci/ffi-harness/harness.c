/* SPDX-License-Identifier: GPL-3.0-or-later
 *
 * A C shell for sukkula-ffi, the spec's "small C harness under ASan"
 * (docs/SPEC.md section 7). run.sh builds the static library for the host
 * and links this against it with -fsanitize=address,undefined; LeakSanitizer
 * checks the whole process at exit.
 *
 * It drives the C ABI exactly as the Qt shell will: from several pthreads,
 * with hostile input, stopping while commands are in flight, and stopping
 * from inside the callback. Inside the callback it checks, on every event,
 * the promises of sukkula.h:
 *
 *  - the string is NUL-terminated UTF-8 JSON (an object), copied at once;
 *  - the callback is not running on a thread that is inside a sukkula_*
 *    call, is never running twice at once, and never runs after
 *    sukkula_stop() has returned.
 *
 * Every check prints "ok" or "FAIL"; any failure makes the exit status 1.
 */

#define _XOPEN_SOURCE 700

#include <errno.h>
#include <ftw.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "sukkula.h"

#define MAX_COMMAND_BYTES (64 * 1024)
#define MAX_IN_FLIGHT 64

static atomic_int failures;

/* Set on a thread while it is inside a sukkula_* call made below. */
static _Thread_local int in_call;

static void check(int ok, const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    fputs(ok ? "ok   " : "FAIL ", stdout);
    vfprintf(stdout, fmt, ap);
    fputc('\n', stdout);
    fflush(stdout);
    va_end(ap);
    if (!ok)
        atomic_fetch_add(&failures, 1);
}

static void sleep_ms(long ms)
{
    struct timespec ts = { ms / 1000, (ms % 1000) * 1000000L };
    while (nanosleep(&ts, &ts) == -1 && errno == EINTR) {
    }
}

/* ---- One shell: the userdata of one engine. --------------------------- */

typedef struct shell shell_t;
typedef void (*hook_fn)(shell_t *s, const char *json);

struct shell {
    pthread_mutex_t lock;
    pthread_cond_t cond;
    char **events;
    size_t count, cap;
    atomic_int busy;
    atomic_int stopped; /* sukkula_stop() has returned */
    atomic_int violations;
    _Atomic(SukkulaEngine *) engine;
    hook_fn hook;
    atomic_int flag;
};

static void shell_init(shell_t *s, hook_fn hook)
{
    memset(s, 0, sizeof *s);
    pthread_mutex_init(&s->lock, NULL);
    pthread_cond_init(&s->cond, NULL);
    s->hook = hook;
}

static void shell_free(shell_t *s)
{
    for (size_t i = 0; i < s->count; i++)
        free(s->events[i]);
    free(s->events);
    pthread_mutex_destroy(&s->lock);
    pthread_cond_destroy(&s->cond);
}

static void violation(shell_t *s, const char *what)
{
    fprintf(stdout, "FAIL callback: %s\n", what);
    atomic_fetch_add(&s->violations, 1);
    atomic_fetch_add(&failures, 1);
}

/* Well-formed UTF-8: no overlongs, surrogates or code points past U+10FFFF. */
static int valid_utf8(const unsigned char *p, size_t n)
{
    size_t i = 0;
    while (i < n) {
        unsigned char c = p[i];
        size_t len;
        uint32_t cp;
        if (c < 0x80) {
            i++;
            continue;
        } else if ((c & 0xE0) == 0xC0) {
            len = 2;
            cp = c & 0x1F;
        } else if ((c & 0xF0) == 0xE0) {
            len = 3;
            cp = c & 0x0F;
        } else if ((c & 0xF8) == 0xF0) {
            len = 4;
            cp = c & 0x07;
        } else {
            return 0;
        }
        if (i + len > n)
            return 0;
        for (size_t k = 1; k < len; k++) {
            if ((p[i + k] & 0xC0) != 0x80)
                return 0;
            cp = (cp << 6) | (p[i + k] & 0x3F);
        }
        if ((len == 2 && cp < 0x80) || (len == 3 && cp < 0x800) ||
            (len == 4 && cp < 0x10000) || cp > 0x10FFFF ||
            (cp >= 0xD800 && cp <= 0xDFFF))
            return 0;
        i += len;
    }
    return 1;
}

static int is_type(const char *json, const char *type)
{
    char prefix[64];
    snprintf(prefix, sizeof prefix, "{\"type\":\"%s\"", type);
    return strncmp(json, prefix, strlen(prefix)) == 0;
}

/* The id of a reply event, or -1. The engine writes the tag, then "id". */
static long long reply_id(const char *json)
{
    if (!is_type(json, "reply"))
        return -1;
    const char *p = strstr(json, "\"id\":");
    return p ? strtoll(p + 5, NULL, 10) : -1;
}

static void on_event(const char *json, void *userdata)
{
    shell_t *s = userdata;
    if (atomic_fetch_add(&s->busy, 1) != 0)
        violation(s, "two callbacks at once");
    if (atomic_load(&s->stopped))
        violation(s, "called after sukkula_stop() returned");
    if (in_call)
        violation(s, "called on a thread inside a sukkula_* call");
    if (json == NULL) {
        violation(s, "a NULL event");
    } else {
        size_t len = strlen(json);
        if (!valid_utf8((const unsigned char *)json, len))
            violation(s, "an event that is not UTF-8");
        if (len < 2 || json[0] != '{' || json[len - 1] != '}')
            violation(s, "an event that is not a JSON object");
        /* Copy at once: the string dies when we return. */
        char *copy = malloc(len + 1);
        if (copy == NULL) {
            violation(s, "out of memory");
        } else {
            memcpy(copy, json, len + 1);
            if (s->hook)
                s->hook(s, copy);
            pthread_mutex_lock(&s->lock);
            if (s->count == s->cap) {
                size_t cap = s->cap ? s->cap * 2 : 64;
                char **grown = realloc(s->events, cap * sizeof *grown);
                if (grown == NULL) {
                    free(copy);
                    copy = NULL;
                } else {
                    s->events = grown;
                    s->cap = cap;
                }
            }
            if (copy != NULL)
                s->events[s->count++] = copy;
            pthread_cond_broadcast(&s->cond);
            pthread_mutex_unlock(&s->lock);
        }
    }
    atomic_fetch_sub(&s->busy, 1);
}

static size_t count_events(shell_t *s)
{
    pthread_mutex_lock(&s->lock);
    size_t n = s->count;
    pthread_mutex_unlock(&s->lock);
    return n;
}

static size_t count_replies(shell_t *s, long long id)
{
    size_t n = 0;
    pthread_mutex_lock(&s->lock);
    for (size_t i = 0; i < s->count; i++)
        if (reply_id(s->events[i]) == id)
            n++;
    pthread_mutex_unlock(&s->lock);
    return n;
}

static size_t count_all_replies(shell_t *s)
{
    size_t n = 0;
    pthread_mutex_lock(&s->lock);
    for (size_t i = 0; i < s->count; i++)
        if (is_type(s->events[i], "reply"))
            n++;
    pthread_mutex_unlock(&s->lock);
    return n;
}

/* Waits up to 20 s for at least `want` replies in all. */
static int wait_replies(shell_t *s, size_t want)
{
    for (int i = 0; i < 2000; i++) {
        if (count_all_replies(s) >= want)
            return 1;
        sleep_ms(10);
    }
    return 0;
}

static int wait_reply(shell_t *s, long long id)
{
    for (int i = 0; i < 2000; i++) {
        if (count_replies(s, id) > 0)
            return 1;
        sleep_ms(10);
    }
    return 0;
}

/* ---- The calls, marked so the callback can tell. ---------------------- */

static SukkulaEngine *start(const char *config, shell_t *s)
{
    in_call = 1;
    SukkulaEngine *h = sukkula_start(config, on_event, s);
    in_call = 0;
    atomic_store(&s->engine, h);
    return h;
}

static int32_t command(SukkulaEngine *h, const char *json)
{
    in_call = 1;
    int32_t rc = sukkula_command(h, json);
    in_call = 0;
    return rc;
}

static void stop(SukkulaEngine *h, shell_t *s)
{
    in_call = 1;
    sukkula_stop(h);
    in_call = 0;
    atomic_store(&s->stopped, 1);
}

static char *get_settings(char *buf, size_t n, long long id)
{
    snprintf(buf, n, "{\"v\":1,\"id\":%lld,\"cmd\":{\"type\":\"get_settings\"}}", id);
    return buf;
}

/* ---- Scratch directories. --------------------------------------------- */

static char base_dir[256];

static int remove_entry(const char *path, const struct stat *st, int flag, struct FTW *ftw)
{
    (void)st;
    (void)flag;
    (void)ftw;
    return remove(path);
}

static void make_config(char *buf, size_t n, const char *name)
{
    snprintf(buf, n,
             "{\"v\":1,\"data_dir\":\"%s/%s/data\",\"download_dir\":\"%s/%s/dl\","
             "\"device_model\":\"C Harness\"}",
             base_dir, name, base_dir, name);
}

/* ---- The checks. ------------------------------------------------------- */

static void test_version(void)
{
    const char *a = sukkula_version();
    const char *b = sukkula_version();
    int dots = 0;
    for (const char *p = a; p && *p; p++)
        dots += *p == '.';
    check(a != NULL && a == b && dots == 2, "version is a static x.y.z string (%s)",
          a ? a : "NULL");
}

static void test_start_failures(void)
{
    char config[1024];
    make_config(config, sizeof config, "fail");

    SukkulaEngine *h = sukkula_start(config, NULL, NULL);
    check(h == NULL, "start with a NULL callback returns NULL");

    char *big = malloc(MAX_COMMAND_BYTES + 100);
    memset(big, ' ', MAX_COMMAND_BYTES + 99);
    big[MAX_COMMAND_BYTES + 99] = '\0';
    memcpy(big, "{\"v\":1}", 7);

    char dupe[1024];
    snprintf(dupe, sizeof dupe,
             "{\"v\":1,\"data_dir\":\"%s/same\",\"download_dir\":\"%s/same\"}", base_dir,
             base_dir);

    struct {
        const char *config;
        const char *what;
    } cases[] = {
        { NULL, "a NULL config" },
        { "{\"v\":1,\xff}", "a non-UTF-8 config" },
        { big, "a config over 64 KiB" },
        { "not json", "a malformed config" },
        { "{\"v\":1,\"data_dir\":\"rel\",\"download_dir\":\"/x\"}", "a relative path" },
        { "{\"v\":7,\"data_dir\":\"/a/b\",\"download_dir\":\"/a/c\"}", "another version" },
        { dupe, "one directory for data and downloads" },
    };
    for (size_t i = 0; i < sizeof cases / sizeof cases[0]; i++) {
        shell_t s;
        shell_init(&s, NULL);
        h = start(cases[i].config, &s);
        int one_fatal = count_events(&s) == 1 && is_type(s.events[0], "fatal");
        check(h == NULL && one_fatal && atomic_load(&s.violations) == 0,
              "start with %s: NULL and exactly one fatal event", cases[i].what);
        shell_free(&s);
    }
    free(big);
}

static void test_start_and_commands(void)
{
    char config[1024], buf[256];
    make_config(config, sizeof config, "commands");
    shell_t s;
    shell_init(&s, NULL);
    SukkulaEngine *h = start(config, &s);
    check(h != NULL, "start with a good config");
    if (h == NULL) {
        shell_free(&s);
        return;
    }
    check(count_events(&s) == 3 && is_type(s.events[0], "started") &&
              is_type(s.events[1], "settings") && is_type(s.events[2], "receiving"),
          "started, settings, receiving arrive before sukkula_start returns");

    check(command(NULL, get_settings(buf, sizeof buf, 1)) == SUKKULA_ERR_NULL,
          "command on a NULL engine");
    check(command(h, NULL) == SUKKULA_ERR_NULL, "a NULL command");
    check(command(h, "{\"v\":1,\"id\":2,\xc3\x28}") == SUKKULA_ERR_UTF8, "a non-UTF-8 command");

    char *big = malloc(MAX_COMMAND_BYTES + 2);
    memset(big, 'x', MAX_COMMAND_BYTES + 1);
    big[MAX_COMMAND_BYTES + 1] = '\0';
    check(command(h, big) == SUKKULA_ERR_TOO_LONG, "a command of 64 KiB + 1");
    /* Exactly 64 KiB: taken, and answered as malformed. */
    const char *head = "{\"v\":1,\"id\":3,\"pad\":\"";
    size_t hl = strlen(head);
    memcpy(big, head, hl);
    memset(big + hl, 'a', MAX_COMMAND_BYTES - hl - 2);
    memcpy(big + MAX_COMMAND_BYTES - 2, "\"}", 3);
    check(strlen(big) == MAX_COMMAND_BYTES && command(h, big) == SUKKULA_OK,
          "a command of exactly 64 KiB is taken");
    check(wait_reply(&s, 3), "and answered");
    free(big);

    SukkulaEngine *garbage = (SukkulaEngine *)(uintptr_t)((uintptr_t)h + 1);
    check(command(garbage, get_settings(buf, sizeof buf, 4)) == SUKKULA_ERR_NULL,
          "a handle this library never gave out");

    check(command(h, "nonsense") == SUKKULA_OK && wait_reply(&s, 0),
          "a malformed command is answered with id 0");
    check(command(h, get_settings(buf, sizeof buf, 5)) == SUKKULA_OK && wait_reply(&s, 5),
          "get_settings is answered");

    sleep_ms(50);
    check(count_replies(&s, 3) == 1 && count_replies(&s, 0) == 1 && count_replies(&s, 5) == 1 &&
              count_replies(&s, 1) == 0 && count_replies(&s, 2) == 0 && count_replies(&s, 4) == 0,
          "exactly one reply per taken command, none for refused ones");

    stop(h, &s);
    size_t n = count_events(&s);
    check(command(h, get_settings(buf, sizeof buf, 6)) == SUKKULA_ERR_NULL,
          "a stopped handle is refused");
    sukkula_stop(h);
    sukkula_stop(NULL);
    sleep_ms(30);
    check(count_events(&s) == n && atomic_load(&s.violations) == 0,
          "stopping twice and stopping NULL are harmless");
    shell_free(&s);
}

/* ---- Commands from several threads. ------------------------------------ */

#define THREADS 8
#define EACH 200

typedef struct {
    SukkulaEngine *h;
    int index;
    long long taken[EACH];
    int ntaken;
    pthread_barrier_t *barrier;
} worker_t;

static void *worker(void *arg)
{
    worker_t *w = arg;
    char buf[256];
    pthread_barrier_wait(w->barrier);
    for (int n = 0; n < EACH; n++) {
        long long id = 1000 + (long long)w->index * EACH + n;
        if (n % 3 == 0)
            snprintf(buf, sizeof buf, "{\"v\":1,\"id\":%lld,\"cmd\":{\"type\":\"nope\"}}", id);
        else if (n % 3 == 1)
            snprintf(buf, sizeof buf,
                     "{\"v\":1,\"id\":%lld,\"cmd\":{\"type\":\"cancel\",\"transfer\":%d}}", id, n);
        else
            get_settings(buf, sizeof buf, id);
        int32_t rc = command(w->h, buf);
        if (rc == SUKKULA_OK)
            w->taken[w->ntaken++] = id;
        else if (rc != SUKKULA_ERR_BUSY)
            check(0, "thread %d: command returned %d", w->index, rc);
    }
    return NULL;
}

static void test_threads(void)
{
    char config[1024];
    make_config(config, sizeof config, "threads");
    shell_t s;
    shell_init(&s, NULL);
    SukkulaEngine *h = start(config, &s);
    check(h != NULL, "start for the thread test");
    if (h == NULL) {
        shell_free(&s);
        return;
    }
    pthread_barrier_t barrier;
    pthread_barrier_init(&barrier, NULL, THREADS);
    static worker_t workers[THREADS];
    pthread_t threads[THREADS];
    for (int i = 0; i < THREADS; i++) {
        workers[i] = (worker_t){ .h = h, .index = i, .barrier = &barrier };
        pthread_create(&threads[i], NULL, worker, &workers[i]);
    }
    size_t taken = 0;
    for (int i = 0; i < THREADS; i++) {
        pthread_join(threads[i], NULL);
        taken += (size_t)workers[i].ntaken;
    }
    pthread_barrier_destroy(&barrier);
    check(taken > 0 && wait_replies(&s, taken), "%zu commands from %d threads all answered",
          taken, THREADS);
    sleep_ms(50);
    int exact = count_all_replies(&s) == taken;
    for (int i = 0; exact && i < THREADS; i++)
        for (int k = 0; exact && k < workers[i].ntaken; k++)
            exact = count_replies(&s, workers[i].taken[k]) == 1;
    check(exact, "each exactly once");
    stop(h, &s);
    check(atomic_load(&s.violations) == 0, "no callback rule broken");
    shell_free(&s);
}

/* ---- Stopping while other threads send commands. ----------------------- */

typedef struct {
    SukkulaEngine *h;
    atomic_int *quit;
    atomic_int *stopped;
    atomic_long *next;
    atomic_int *late;
} hammer_t;

static void *hammer(void *arg)
{
    hammer_t *w = arg;
    char buf[256];
    while (!atomic_load(w->quit)) {
        int was_stopped = atomic_load(w->stopped);
        int32_t rc = command(w->h, get_settings(buf, sizeof buf, atomic_fetch_add(w->next, 1)));
        if (was_stopped && rc != SUKKULA_ERR_NULL)
            atomic_fetch_add(w->late, 1);
        if (rc != SUKKULA_OK && rc != SUKKULA_ERR_BUSY && rc != SUKKULA_ERR_NULL)
            atomic_fetch_add(w->late, 1);
    }
    return NULL;
}

static void test_stop_while_hammering(void)
{
    char config[1024];
    for (int round = 0; round < 5; round++) {
        char name[32];
        snprintf(name, sizeof name, "hammer%d", round);
        make_config(config, sizeof config, name);
        shell_t s;
        shell_init(&s, NULL);
        SukkulaEngine *h = start(config, &s);
        if (h == NULL) {
            check(0, "start for the stop test");
            shell_free(&s);
            return;
        }
        atomic_int quit = 0, stopped = 0, late = 0;
        atomic_long next = 1;
        hammer_t w = { h, &quit, &stopped, &next, &late };
        pthread_t threads[4];
        for (int i = 0; i < 4; i++)
            pthread_create(&threads[i], NULL, hammer, &w);
        sleep_ms(20 + round * 10);
        stop(h, &s);
        atomic_store(&stopped, 1);
        sleep_ms(30);
        atomic_store(&quit, 1);
        for (int i = 0; i < 4; i++)
            pthread_join(threads[i], NULL);
        size_t n = count_events(&s);
        sleep_ms(30);
        check(count_events(&s) == n && atomic_load(&late) == 0 &&
                  atomic_load(&s.violations) == 0 && atomic_load(&next) > 10,
              "round %d: stop while 4 threads send commands (%ld sent)", round,
              (long)atomic_load(&next));
        shell_free(&s);
    }
}

/* ---- The callback calls back in. --------------------------------------- */

static void command_from_callback(shell_t *s, const char *json)
{
    char buf[256];
    if (reply_id(json) == 1) {
        int32_t rc = command(atomic_load(&s->engine), get_settings(buf, sizeof buf, 2));
        if (rc != SUKKULA_OK)
            violation(s, "sukkula_command from the callback was refused");
    }
}

static void stop_from_callback(shell_t *s, const char *json)
{
    char buf[256];
    if (reply_id(json) == 1) {
        SukkulaEngine *h = atomic_load(&s->engine);
        stop(h, s);
        if (command(h, get_settings(buf, sizeof buf, 99)) != SUKKULA_ERR_NULL)
            violation(s, "the handle outlived a stop from the callback");
        atomic_store(&s->flag, 1);
    }
}

static void test_callback_calls_in(void)
{
    char config[1024], buf[256];
    make_config(config, sizeof config, "reenter");
    shell_t s;
    shell_init(&s, command_from_callback);
    SukkulaEngine *h = start(config, &s);
    check(h != NULL && command(h, get_settings(buf, sizeof buf, 1)) == SUKKULA_OK &&
              wait_reply(&s, 2),
          "the callback may call sukkula_command");
    stop(h, &s);
    shell_free(&s);

    make_config(config, sizeof config, "selfstop");
    shell_init(&s, stop_from_callback);
    h = start(config, &s);
    int taken = h != NULL;
    for (long long id = 1; taken && id <= 5; id++)
        taken = command(h, get_settings(buf, sizeof buf, id)) == SUKKULA_OK;
    for (int i = 0; i < 2000 && !atomic_load(&s.flag); i++)
        sleep_ms(5);
    int returned = atomic_load(&s.flag);
    size_t n = count_events(&s);
    sleep_ms(100);
    check(taken && returned && count_events(&s) == n && atomic_load(&s.violations) == 0,
          "sukkula_stop from inside the callback returns and ends the callbacks");
    sukkula_stop(h);
    shell_free(&s);
}

/* ---- A UI that stops reading. ------------------------------------------ */

static void hold_while_flagged(shell_t *s, const char *json)
{
    if (is_type(json, "reply"))
        while (atomic_load(&s->flag))
            sleep_ms(1);
}

static void test_busy(void)
{
    char config[1024], buf[256];
    make_config(config, sizeof config, "busy");
    shell_t s;
    shell_init(&s, hold_while_flagged);
    atomic_store(&s.flag, 1);
    SukkulaEngine *h = start(config, &s);
    if (h == NULL) {
        check(0, "start for the busy test");
        shell_free(&s);
        return;
    }
    int taken = 0;
    for (long long id = 0; id < MAX_IN_FLIGHT; id++)
        taken += command(h, get_settings(buf, sizeof buf, id)) == SUKKULA_OK;
    /* Every reply waits behind the held callback, so every slot is used. */
    int32_t rc = command(h, get_settings(buf, sizeof buf, 9999));
    int32_t rc2 = command(h, "not even json");
    check(taken == MAX_IN_FLIGHT && rc == SUKKULA_ERR_BUSY && rc2 == SUKKULA_ERR_BUSY,
          "the %d-th command waiting for its reply gets SUKKULA_ERR_BUSY", MAX_IN_FLIGHT + 1);
    atomic_store(&s.flag, 0);
    check(wait_replies(&s, (size_t)taken), "and the %d replies still arrive", taken);
    int again = 0;
    for (int i = 0; i < 200 && !again; i++) {
        again = command(h, get_settings(buf, sizeof buf, 10000)) == SUKKULA_OK;
        if (!again)
            sleep_ms(5);
    }
    check(again && wait_reply(&s, 10000) && count_replies(&s, 9999) == 0,
          "then commands are taken again");
    stop(h, &s);
    check(atomic_load(&s.violations) == 0, "no callback rule broken");
    shell_free(&s);
}

/* ---- Start/stop cycles. ------------------------------------------------ */

static void test_cycles(void)
{
    char config[1024], buf[256];
    int ok = 1;
    for (int i = 0; i < 200 && ok; i++) {
        char name[32];
        snprintf(name, sizeof name, "cycle%d", i);
        make_config(config, sizeof config, name);
        shell_t s;
        shell_init(&s, NULL);
        SukkulaEngine *h = start(config, &s);
        ok = h != NULL && command(h, get_settings(buf, sizeof buf, 1)) == SUKKULA_OK &&
             wait_reply(&s, 1);
        stop(h, &s);
        ok = ok && atomic_load(&s.violations) == 0;
        shell_free(&s);
    }
    check(ok, "200 start/stop cycles (LeakSanitizer checks them at exit)");
}

int main(void)
{
    const char *tmp = getenv("TMPDIR");
    int n = snprintf(base_dir, sizeof base_dir, "%s/sukkula-ffi-harness-XXXXXX",
                     tmp && *tmp ? tmp : "/tmp");
    if (n < 0 || (size_t)n >= sizeof base_dir) {
        fputs("TMPDIR is too long\n", stderr);
        return 2;
    }
    if (mkdtemp(base_dir) == NULL) {
        perror("mkdtemp");
        return 2;
    }

    test_version();
    test_start_failures();
    test_start_and_commands();
    test_threads();
    test_stop_while_hammering();
    test_callback_calls_in();
    test_busy();
    test_cycles();

    nftw(base_dir, remove_entry, 16, FTW_DEPTH | FTW_PHYS);
    int failed = atomic_load(&failures);
    printf("%s: %d failure(s)\n", failed ? "FAIL" : "PASS", failed);
    return failed ? 1 : 0;
}
