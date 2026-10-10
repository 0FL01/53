/* Exercise the actual staged runtime seams with real host TCP FDs. Only the
 * stream/QUIC owner and deliberate syscall failures are substituted. */
#include "slipstream_runtime.h"
#include "slipstream.h"
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/tcp.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

enum fault { NONE, SET_FAIL, GET_FAIL, READ_ZERO, SHORT_READ,
    NONBLOCK_FAIL, CREATE_FAIL, CTX_FAIL, CONNECT_FAIL };
static enum fault fault;
static int observed_fd, set_calls, get_calls, close_calls, nonblock_calls;
static int create_calls, abort_calls, reset_calls, stop_calls, event_calls;
static ss_stream owned;
static ss_conn head;
volatile sig_atomic_t should_shutdown;

static int nodelay(int fd) {
    int value = -1;
    socklen_t length = sizeof(value);
    assert(getsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &value, &length) == 0);
    assert(length == sizeof(value));
    return value;
}
static int checked_set(int fd, int level, int option, const void *value, socklen_t length) {
    assert(fd == observed_fd && level == IPPROTO_TCP && option == TCP_NODELAY);
    assert(length == sizeof(int) && *(const int *)value == 1);
    set_calls++;
    if (fault == SET_FAIL) { errno = EIO; return -1; }
    return setsockopt(fd, level, option, value, length);
}
static int checked_get(int fd, int level, int option, void *value, socklen_t *length) {
    assert(fd == observed_fd && level == IPPROTO_TCP && option == TCP_NODELAY);
    assert(set_calls == 1 && *length == sizeof(int));
    get_calls++;
    if (fault == GET_FAIL) { errno = EIO; return -1; }
    int ret = getsockopt(fd, level, option, value, length);
    if (fault == READ_ZERO) *(int *)value = 0;
    if (fault == SHORT_READ) *length = sizeof(int) - 1;
    return ret;
}
static int checked_close(int fd) {
    if (fd == observed_fd) close_calls++;
    return close(fd);
}
static int checked_nonblock(int fd) {
    assert(fd == observed_fd);
#ifdef DMSG_PROBE_TCP_NODELAY
    assert(set_calls == 1 && get_calls == 1 && nodelay(fd) == 1);
#else
    assert(set_calls == 0 && get_calls == 0 && nodelay(fd) == 0);
#endif
    nonblock_calls++;
    if (fault == NONBLOCK_FAIL) return -1;
    return ss_nonblock(fd);
}
static ss_stream *checked_create(ss_conn *c, int fd, uint64_t id) {
    assert(nonblock_calls == 1 && close_calls == 0);
    create_calls++;
    if (fault == CREATE_FAIL) return NULL;
    owned = (ss_stream){.fd = fd, .id = id};
    c->streams = &owned; c->count++;
    return &owned;
}
static void checked_service(ss_conn *c, uint64_t now) { (void)c; (void)now; }
static size_t checked_budget(ss_conn *c, int path, uint64_t now) {
    (void)c; (void)path; (void)now; return 0;
}

#ifdef TEST_CLIENT
static int checked_accept(int listener, struct sockaddr *address, socklen_t *length) {
    int fd = accept(listener, address, length);
    if (fd >= 0) observed_fd = fd;
    return fd;
}
#define accept checked_accept
#else
static int checked_socket(int family, int type, int protocol) {
    observed_fd = socket(family, type, protocol);
    return observed_fd;
}
static picoquic_quic_t *checked_quic(picoquic_cnx_t *cnx) { (void)cnx; return NULL; }
static void *checked_context(picoquic_quic_t *quic) { (void)quic; return &head; }
static int checked_reset(picoquic_cnx_t *cnx, uint64_t id, uint64_t error) {
    (void)cnx; assert(id == 4 && error == SLIPSTREAM_INTERNAL_ERROR);
    assert(close_calls == 1); reset_calls++; return 0;
}
static int checked_stop(picoquic_cnx_t *cnx, uint64_t id, uint64_t error) {
    (void)cnx; assert(id == 4 && error == SLIPSTREAM_INTERNAL_ERROR);
    assert(reset_calls == 1); stop_calls++; return 0;
}
static int checked_app_ctx(picoquic_cnx_t *cnx, uint64_t id, void *stream) {
    (void)cnx; assert(id == 4 && stream == &owned && close_calls == 0);
    return fault == CTX_FAIL ? -1 : 0;
}
static void checked_abort(ss_conn *c, ss_stream *s) {
    assert(s == &owned && c->count == 1);
    abort_calls++; checked_close(s->fd); c->count = 0; c->streams = NULL;
}
static int checked_connect(int fd, const struct sockaddr *address, socklen_t length) {
    assert(create_calls == 1 && close_calls == 0);
    if (fault == CONNECT_FAIL) { errno = ECONNREFUSED; return -1; }
    return connect(fd, address, length);
}
static int checked_event(ss_conn *c, uint64_t id, uint8_t *bytes, size_t length,
    picoquic_call_back_event_t event, ss_stream *s) {
    (void)c; (void)bytes; (void)length; (void)event;
    assert(id == 4 && s == &owned); event_calls++; return 0;
}
#define socket checked_socket
#define connect checked_connect
#define picoquic_get_quic_ctx checked_quic
#define picoquic_get_default_callback_context checked_context
#define picoquic_reset_stream checked_reset
#define picoquic_stop_sending checked_stop
#define picoquic_set_app_stream_ctx checked_app_ctx
#define ss_stream_abort checked_abort
#define ss_stream_event checked_event
#endif
#define setsockopt checked_set
#define getsockopt checked_get
#define close checked_close
#define ss_nonblock checked_nonblock
#define ss_stream_create checked_create
#define ss_conn_service checked_service
#define ss_conn_poll_budget checked_budget
#ifdef TEST_CLIENT
#include "slipstream_client_runtime.c"
#else
#include "slipstream_server_runtime.c"
#endif
#undef accept
#undef socket
#undef connect
#undef close
#undef setsockopt
#undef getsockopt
#undef ss_nonblock
#undef ss_stream_create

static void run(enum fault injected) {
    fault = injected;
    observed_fd = -1;
    set_calls = get_calls = close_calls = nonblock_calls = create_calls = 0;
    abort_calls = reset_calls = stop_calls = event_calls = 0;
    int listener = socket(AF_INET, SOCK_STREAM, 0); assert(listener >= 0);
    struct sockaddr_in address = {.sin_family = AF_INET};
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    assert(bind(listener, (struct sockaddr *)&address, sizeof(address)) == 0);
    assert(listen(listener, 1) == 0);
    socklen_t length = sizeof(address);
    assert(getsockname(listener, (struct sockaddr *)&address, &length) == 0);
#ifdef TEST_CLIENT
    int peer = socket(AF_INET, SOCK_STREAM, 0); assert(peer >= 0);
    assert(connect(peer, (struct sockaddr *)&address, length) == 0);
    assert(ss_nonblock(listener) == 0);
    client_runtime runtime = {0};
    runtime.loop.listener = listener; runtime.loop.head.ready = true;
    assert(loop_callback(NULL, picoquic_packet_loop_before_select, &runtime, NULL) == 0);
    ss_conn *c = &runtime.loop.head;
#else
    ss_conn connection = {0};
    memcpy(&connection.target, &address, length);
    ss_conn *c = &connection;
    assert(server_callback((picoquic_cnx_t *)1, 4, NULL, 0,
        picoquic_callback_stream_data, c, NULL) == 0);
#endif
    assert(observed_fd >= 0);
    if (injected == NONE) {
        assert(c->count == 1 && create_calls == 1 && close_calls == 0);
#ifdef DMSG_PROBE_TCP_NODELAY
        assert(nodelay(observed_fd) == 1 && set_calls == 1 && get_calls == 1);
#else
        assert(nodelay(observed_fd) == 0 && set_calls == 0 && get_calls == 0);
#endif
        close(observed_fd); /* The test stream owner performs normal cleanup. */
    } else {
        assert(c->count == 0 && close_calls == 1);
        assert(fcntl(observed_fd, F_GETFD) == -1 && errno == EBADF);
        if (injected <= SHORT_READ) assert(nonblock_calls == 0 && create_calls == 0);
        if (injected == NONBLOCK_FAIL) assert(create_calls == 0);
#ifndef TEST_CLIENT
        if (injected == CTX_FAIL || injected == CONNECT_FAIL)
            assert(abort_calls == 1);
        else assert(reset_calls == 1 && stop_calls == 1 && abort_calls == 0);
        assert(event_calls == 0);
#endif
    }
#ifdef TEST_CLIENT
    close(peer);
#endif
    close(listener);
}

int main(void) {
    run(NONE);
#ifdef DMSG_PROBE_TCP_NODELAY
    run(SET_FAIL); run(GET_FAIL); run(READ_ZERO); run(SHORT_READ);
#endif
    run(NONBLOCK_FAIL); run(CREATE_FAIL);
#ifndef TEST_CLIENT
    run(CTX_FAIL); run(CONNECT_FAIL);
#endif
    /* Baseline AF_UNIX ownership path never invokes the TCP-only helper. */
    int pair[2]; assert(socketpair(AF_UNIX, SOCK_STREAM, 0, pair) == 0);
    assert(ss_nonblock(pair[0]) == 0 && ss_nonblock(pair[1]) == 0);
    close(pair[0]); close(pair[1]);
    puts("real TCP seam/readback, rejection ownership and AF_UNIX baseline passed");
    return 0;
}
