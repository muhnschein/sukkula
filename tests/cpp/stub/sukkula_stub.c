/* SPDX-License-Identifier: GPL-3.0-or-later
 *
 * A stand-in for the Rust engine, for host tests of the Qt bridge and the
 * whole shell while the real static library is built elsewhere. It keeps
 * the contract of crates/sukkula-ffi/include/sukkula.h:
 *
 *  - the callback runs on a thread of the stub's, never the caller's;
 *  - sukkula_start() emits "started", "settings" and "receiving" before it
 *    returns, or one "fatal" and returns NULL;
 *  - every command taken is answered by one "reply" with its id;
 *  - after sukkula_stop() returns the callback is never called again;
 *  - NULL, over-long and non-UTF-8 input is refused with the header's codes.
 *
 * Never linked into the app.
 */
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "sukkula.h"
#include "sukkula_stub.h"

enum {
    MAX_COMMAND = 64 * 1024,
    MAX_QUEUE = 1024,
    BURST = 5000,
    OVERSIZE = 2 * 1024 * 1024
};

enum job_kind { JOB_EMIT, JOB_BURST, JOB_OVERSIZE };

struct job {
    enum job_kind kind;
    long delay_ms;
    char *text;
};

struct SukkulaEngine {
    pthread_t thread;
    pthread_mutex_t lock;
    pthread_cond_t wake;
    struct job queue[MAX_QUEUE];
    size_t head;
    size_t count;
    int stopping;
    sukkula_event_cb callback;
    void *userdata;
};

static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;
static char g_last_config[MAX_COMMAND + 1];
static int g_fail_next;

static const char *const START_EVENTS[] = {
    "{\"type\":\"started\",\"version\":\"0.0.0-stub\",\"api\":1,"
    "\"protocols\":[\"local_send\",\"quick_share\",\"wormhole\",\"bluetooth\"]}",
    "{\"type\":\"settings\",\"settings\":{\"device_name\":\"\","
    "\"localsend\":{\"enabled\":true,\"pin\":null},"
    "\"quickshare\":{\"enabled\":true,\"visibility\":\"everyone\",\"ble_nudge\":true},"
    "\"wormhole\":{\"mailbox_url\":null,\"relay_url\":null},"
    "\"bluetooth\":{\"enabled\":true},\"logging\":false},"
    "\"effective_device_name\":\"Stub Phone\"}",
    "{\"type\":\"receiving\",\"on\":false,\"protocols\":["
    "{\"protocol\":\"local_send\",\"state\":\"off\"},"
    "{\"protocol\":\"quick_share\",\"state\":\"off\"},"
    "{\"protocol\":\"wormhole\",\"state\":\"send_only\"},"
    "{\"protocol\":\"bluetooth\",\"state\":\"send_only\"}]}",
};

const char *sukkula_stub_last_config(void)
{
    return g_last_config;
}

void sukkula_stub_fail_next_start(int fail)
{
    pthread_mutex_lock(&g_lock);
    g_fail_next = fail;
    pthread_mutex_unlock(&g_lock);
}

const char *sukkula_version(void)
{
    return "0.0.0-stub";
}

static void sleep_ms(long ms)
{
    struct timespec ts;
    ts.tv_sec = ms / 1000;
    ts.tv_nsec = (ms % 1000) * 1000000L;
    nanosleep(&ts, NULL);
}

static int valid_utf8(const unsigned char *s, size_t n)
{
    size_t i = 0;
    while (i < n) {
        unsigned char c = s[i];
        size_t extra;
        unsigned int cp;
        if (c < 0x80) {
            i++;
            continue;
        } else if ((c & 0xe0) == 0xc0) {
            extra = 1;
            cp = c & 0x1f;
        } else if ((c & 0xf0) == 0xe0) {
            extra = 2;
            cp = c & 0x0f;
        } else if ((c & 0xf8) == 0xf0) {
            extra = 3;
            cp = c & 0x07;
        } else {
            return 0;
        }
        if (i + extra >= n) {
            return 0;
        }
        for (size_t k = 1; k <= extra; k++) {
            if ((s[i + k] & 0xc0) != 0x80) {
                return 0;
            }
            cp = (cp << 6) | (s[i + k] & 0x3f);
        }
        if ((extra == 1 && cp < 0x80) || (extra == 2 && cp < 0x800) || (extra == 3 && cp < 0x10000)
            || cp > 0x10ffff || (cp >= 0xd800 && cp <= 0xdfff)) {
            return 0;
        }
        i += extra + 1;
    }
    return 1;
}

/* The "id" of a command, best effort, as the real engine recovers it. */
static unsigned long long command_id(const char *json)
{
    const char *at = strstr(json, "\"id\":");
    unsigned long long id = 0;
    if (!at) {
        return 0;
    }
    at += 5;
    for (int digits = 0; *at >= '0' && *at <= '9' && digits < 18; at++, digits++) {
        id = id * 10 + (unsigned long long)(*at - '0');
    }
    return id;
}

static void run_start_events(sukkula_event_cb callback, void *userdata, int fail)
{
    if (fail) {
        callback("{\"type\":\"fatal\",\"error\":{\"code\":\"storage\",\"message\":\"stub told to fail\"}}",
                 userdata);
        return;
    }
    for (size_t i = 0; i < sizeof START_EVENTS / sizeof START_EVENTS[0]; i++) {
        callback(START_EVENTS[i], userdata);
    }
}

struct start_args {
    sukkula_event_cb callback;
    void *userdata;
    int fail;
};

static void *start_thread(void *arg)
{
    struct start_args *a = arg;
    run_start_events(a->callback, a->userdata, a->fail);
    return NULL;
}

static int stopping(SukkulaEngine *e)
{
    pthread_mutex_lock(&e->lock);
    int s = e->stopping;
    pthread_mutex_unlock(&e->lock);
    return s;
}

static void run_job(SukkulaEngine *e, struct job *job)
{
    if (job->delay_ms > 0) {
        sleep_ms(job->delay_ms);
    }
    switch (job->kind) {
    case JOB_EMIT:
        e->callback(job->text, e->userdata);
        break;
    case JOB_BURST: {
        char buf[128];
        for (int i = 0; i < BURST && !stopping(e); i++) {
            snprintf(buf, sizeof buf, "{\"type\":\"transfer_progress\",\"transfer\":1,\"bytes\":%d,\"total\":%d}",
                     i, BURST);
            e->callback(buf, e->userdata);
        }
        e->callback(job->text, e->userdata);
        break;
    }
    case JOB_OVERSIZE: {
        char *big = malloc(OVERSIZE + 64);
        if (big) {
            size_t n = (size_t)snprintf(big, 64, "{\"type\":\"text_received\",\"text\":\"");
            memset(big + n, 'a', OVERSIZE);
            memcpy(big + n + OVERSIZE, "\"}", 3);
            e->callback(big, e->userdata);
            free(big);
        }
        e->callback(job->text, e->userdata);
        break;
    }
    }
}

static void *engine_thread(void *arg)
{
    SukkulaEngine *e = arg;
    for (;;) {
        pthread_mutex_lock(&e->lock);
        while (!e->stopping && e->count == 0) {
            pthread_cond_wait(&e->wake, &e->lock);
        }
        if (e->stopping) {
            pthread_mutex_unlock(&e->lock);
            break;
        }
        struct job job = e->queue[e->head];
        e->head = (e->head + 1) % MAX_QUEUE;
        e->count--;
        pthread_mutex_unlock(&e->lock);
        run_job(e, &job);
        free(job.text);
    }
    return NULL;
}

SukkulaEngine *sukkula_start(const char *config_json, sukkula_event_cb callback, void *userdata)
{
    if (!callback) {
        return NULL;
    }
    pthread_mutex_lock(&g_lock);
    if (config_json && strnlen(config_json, MAX_COMMAND + 1) <= MAX_COMMAND) {
        strcpy(g_last_config, config_json);
    } else {
        g_last_config[0] = '\0';
    }
    int fail = g_fail_next || !config_json;
    g_fail_next = 0;
    pthread_mutex_unlock(&g_lock);

    /* The first events come from a thread that is not the caller's, and
     * all of them before this function returns. */
    struct start_args args = { callback, userdata, fail };
    pthread_t first;
    if (pthread_create(&first, NULL, start_thread, &args) != 0) {
        return NULL;
    }
    pthread_join(first, NULL);
    if (fail) {
        return NULL;
    }

    SukkulaEngine *e = calloc(1, sizeof *e);
    if (!e) {
        return NULL;
    }
    e->callback = callback;
    e->userdata = userdata;
    pthread_mutex_init(&e->lock, NULL);
    pthread_cond_init(&e->wake, NULL);
    if (pthread_create(&e->thread, NULL, engine_thread, e) != 0) {
        pthread_cond_destroy(&e->wake);
        pthread_mutex_destroy(&e->lock);
        free(e);
        return NULL;
    }
    return e;
}

int32_t sukkula_command(SukkulaEngine *engine, const char *command_json)
{
    if (!engine || !command_json) {
        return SUKKULA_ERR_NULL;
    }
    size_t n = strnlen(command_json, MAX_COMMAND + 1);
    if (n > MAX_COMMAND) {
        return SUKKULA_ERR_TOO_LONG;
    }
    if (!valid_utf8((const unsigned char *)command_json, n)) {
        return SUKKULA_ERR_UTF8;
    }
    if (strstr(command_json, "\"stub\":\"busy\"")) {
        return SUKKULA_ERR_BUSY;
    }
    struct job job = { JOB_EMIT, 0, NULL };
    if (strstr(command_json, "\"stub\":\"burst\"")) {
        job.kind = JOB_BURST;
    } else if (strstr(command_json, "\"stub\":\"oversize\"")) {
        job.kind = JOB_OVERSIZE;
    } else if (strstr(command_json, "\"stub\":\"slow\"")) {
        job.delay_ms = 200;
    }
    job.text = malloc(96);
    if (!job.text) {
        return SUKKULA_ERR_PANIC;
    }
    snprintf(job.text, 96, "{\"type\":\"reply\",\"id\":%llu,\"ok\":true}", command_id(command_json));

    pthread_mutex_lock(&engine->lock);
    if (engine->count == MAX_QUEUE) {
        pthread_mutex_unlock(&engine->lock);
        free(job.text);
        return SUKKULA_ERR_BUSY;
    }
    engine->queue[(engine->head + engine->count) % MAX_QUEUE] = job;
    engine->count++;
    pthread_cond_signal(&engine->wake);
    pthread_mutex_unlock(&engine->lock);
    return SUKKULA_OK;
}

void sukkula_stop(SukkulaEngine *engine)
{
    if (!engine) {
        return;
    }
    pthread_mutex_lock(&engine->lock);
    engine->stopping = 1;
    pthread_cond_broadcast(&engine->wake);
    pthread_mutex_unlock(&engine->lock);
    pthread_join(engine->thread, NULL);
    while (engine->count > 0) {
        free(engine->queue[engine->head].text);
        engine->head = (engine->head + 1) % MAX_QUEUE;
        engine->count--;
    }
    pthread_cond_destroy(&engine->wake);
    pthread_mutex_destroy(&engine->lock);
    free(engine);
}
