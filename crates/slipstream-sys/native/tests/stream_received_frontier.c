/* Real pinned STREAM decode/reassembly and bridge flush. Public in-memory bytes
 * and deterministic simulated time; not network/carrier or clock acceptance. */
#include "embed.h"
#include "slipstream_runtime.h"
#include <arpa/inet.h>
#include <assert.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/time.h>
#include <unistd.h>

dmsg_control *dmsg_control_new(void);
void dmsg_control_free(dmsg_control *control);

static int callback(picoquic_cnx_t *cnx, uint64_t id, uint8_t *bytes, size_t length,
    picoquic_call_back_event_t event, void *context, void *stream_context) {
    (void)cnx;
    if (event != picoquic_callback_stream_data && event != picoquic_callback_stream_fin) return 0;
    return ss_stream_event(context, id, bytes, length, event, stream_context);
}
static dmsg_stream_snapshot capture(dmsg_control *control, ss_conn *conn, uint64_t now) {
    dmsg_stream_publish(control, conn, now);
    dmsg_stream_snapshot out;
    assert(dmsg_control_stream_snapshot(control, &out, sizeof(out)) == 1);
    assert(out.schema_version == 2 && out.rx_pending == 0);
    assert(out.rx_contiguous == out.rx_flushed && !out.remote_fin);
    return out;
}
static void input(picoquic_cnx_t *cnx, const uint8_t *frame, size_t length, uint64_t now) {
    assert(picoquic_decode_stream_frame(cnx, frame, frame + length, NULL, now) == frame + length);
}
static void read_bytes(int fd, const char *expected, size_t length) {
    char out[12]; assert(length <= sizeof(out));
    size_t read = 0;
    while (read < length) {
        ssize_t n = recv(fd, out + read, length - read, 0);
        assert(n > 0); read += (size_t)n;
    }
    assert(memcmp(out, expected, length) == 0);
}
int main(void) {
    int listener = socket(AF_INET, SOCK_STREAM, 0); assert(listener >= 0);
    struct sockaddr_in address = {.sin_family = AF_INET, .sin_addr.s_addr = htonl(INADDR_LOOPBACK)};
    assert(bind(listener, (struct sockaddr *)&address, sizeof(address)) == 0);
    assert(listen(listener, 1) == 0);
    socklen_t size = sizeof(address);
    assert(getsockname(listener, (struct sockaddr *)&address, &size) == 0);
    int peer = socket(AF_INET, SOCK_STREAM, 0); assert(peer >= 0);
    /* Bounded eventual fixture drain, not a media/transport latency gate. Keep
     * ordinary TCP behavior; kernel delayed ACK/Nagle must not make it flaky. */
    const struct timeval receive_timeout = {.tv_sec = 2};
    assert(setsockopt(peer, SOL_SOCKET, SO_RCVTIMEO, &receive_timeout, sizeof(receive_timeout)) == 0);
    assert(connect(peer, (struct sockaddr *)&address, sizeof(address)) == 0);
    int accepted = accept(listener, NULL, NULL); assert(accepted >= 0); close(listener);
    struct sockaddr_in local = {0}; size = sizeof(local);
    assert(getsockname(peer, (struct sockaddr *)&local, &size) == 0);

    ss_conn conn = {.client = true, .ready = true, .count = 1};
    uint64_t now = 1000;
    picoquic_quic_t *quic = picoquic_create(8, NULL, NULL, NULL, "frontier-fixture",
        callback, &conn, NULL, NULL, NULL, now, &now, NULL, NULL, 0); assert(quic);
    conn.cnx = picoquic_create_cnx(quic, picoquic_null_connection_id, picoquic_null_connection_id,
        (struct sockaddr *)&address, now, 0, "fixture.invalid", "frontier-fixture", 1); assert(conn.cnx);
    picoquic_stream_head_t *qs = picoquic_create_stream(conn.cnx, 0); assert(qs);
    conn.cnx->maxdata_local = qs->maxdata_local = 4096;
    ss_stream stream = {.id = 0, .fd = accepted, .stage = 4, .connected = true};
    conn.streams = &stream; qs->app_stream_ctx = &stream;
    dmsg_control *control = dmsg_control_new(); assert(control);
    dmsg_control_select_stream(control, (UINT64_C(1) << 16) | ntohs(local.sin_port));
    dmsg_stream_snapshot s = capture(control, &conn, now);
    assert(s.rx_highest == 0 && s.rx_contiguous == 0);

    /* All frames carry OFF+LEN, no FIN: highest range end is not a FIN-only mark. */
    const uint8_t tail[] = {0x0e, 0, 8, 4, 'I', 'J', 'K', 'L'};
    const uint8_t head[] = {0x0e, 0, 0, 4, 'A', 'B', 'C', 'D'};
    const uint8_t gap[] = {0x0e, 0, 4, 4, 'E', 'F', 'G', 'H'};
    input(conn.cnx, tail, sizeof(tail), ++now);
    s = capture(control, &conn, now); assert(s.rx_highest == 12 && s.rx_contiguous == 0);
    char byte;
    assert(recv(peer, &byte, 1, MSG_DONTWAIT) == -1 && (errno == EAGAIN || errno == EWOULDBLOCK));
    input(conn.cnx, head, sizeof(head), ++now);
    s = capture(control, &conn, now); assert(s.rx_highest == 12 && s.rx_contiguous == 4);
    read_bytes(peer, "ABCD", 4);
    input(conn.cnx, head, sizeof(head), ++now); /* Duplicate cannot create progress. */
    s = capture(control, &conn, now); assert(s.rx_highest == 12 && s.rx_contiguous == 4);
    assert(conn.cnx->data_received == 12 && !qs->fin_received);
    input(conn.cnx, gap, sizeof(gap), ++now);
    s = capture(control, &conn, now); assert(s.rx_highest == 12 && s.rx_contiguous == 12);
    read_bytes(peer, "EFGHIJKL", 8);
    assert(conn.cnx->data_received == 12 && !qs->fin_received && !stream.remote_fin);

    /* End-offset credit can also advance without buffered suffix bytes. Do not
     * interpret highest-contiguous as a byte count stored in the receive tree. */
    const uint8_t empty_range[] = {0x0e, 0, 16, 0};
    input(conn.cnx, empty_range, sizeof(empty_range), ++now);
    s = capture(control, &conn, now); assert(s.rx_highest == 16 && s.rx_contiguous == 12);
    assert(recv(peer, &byte, 1, MSG_DONTWAIT) == -1 && (errno == EAGAIN || errno == EWOULDBLOCK));

    dmsg_control_select_stream(control, 0);
    assert(!dmsg_control_stream_snapshot(control, &s, sizeof(s)));
    dmsg_control_free(control); picoquic_free(quic); close(peer); close(accepted);
    puts("real STREAM input: 12/0 -> 12/4 -> duplicate12/4 -> gap-fill12/12 -> empty-range16/12; bridge flush exact, no FIN PASS");
    return 0;
}
