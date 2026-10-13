#include "embed.h"
#include "slipstream.h"
#include "slipstream_runtime.h"
#include <picoquic_internal.h>
#include <arpa/inet.h>
#include <stdlib.h>
#include <openssl/crypto.h>
#include <string.h>

int dmsg_native_run(dmsg_control *, const dmsg_config *);
int dmsg_embedded_client(int, address_t *, size_t, const char *, const char *,
    bool, size_t, size_t, const char *, const char *, bool,
    dmsg_control *, const uint8_t *, size_t);

dmsg_control *dmsg_control_new(void) {
    dmsg_control *c = malloc(sizeof(*c));
    if (c) {
        atomic_init(&c->stop, false);
        atomic_init(&c->pin_failed, false);
        atomic_init(&c->snapshot, DMSG_STARTING);
        atomic_init(&c->transport_sequence, 0);
        for (size_t i = 0; i < DMSG_TRANSPORT_WORDS; ++i)
            atomic_init(&c->transport_words[i], 0);
        atomic_init(&c->stream_request, 0);
        atomic_init(&c->stream_sequence, 0);
        atomic_init(&c->stream_match, 0);
        for (size_t i = 0; i < DMSG_STREAM_WORDS; ++i)
            atomic_init(&c->stream_words[i], 0);
    }
    return c;
}
void dmsg_control_free(dmsg_control *c) { free(c); }
void dmsg_control_stop(dmsg_control *c) {
    atomic_store_explicit(&c->stop, true, memory_order_release);
}
int dmsg_cancelled(dmsg_control *c) {
    return atomic_load_explicit(&c->stop, memory_order_acquire);
}
uint64_t dmsg_control_status(dmsg_control *c) {
    return atomic_load_explicit(&c->snapshot, memory_order_acquire);
}
void dmsg_publish(dmsg_control *c, unsigned phase, uint16_t port, int error) {
    if (phase == DMSG_STOPPED || phase == DMSG_FAILED) {
        dmsg_transport_publish(c, NULL, 0);
        dmsg_stream_publish(c, NULL, 0);
    }
    atomic_store_explicit(&c->snapshot,
        phase | ((uint64_t)port << 8) | ((uint64_t)(uint32_t)error << 24),
        memory_order_release);
}
/* All sequence and payload operations are seq_cst. Their single total order
 * makes equal even sequence reads bracket one coherent payload, including on
 * weakly ordered hosts. No shared plain bytes, locks, allocation or fallback.
 * Only the native worker writes; a read costs at most 3 * (155 + 2) loads. */
static void transport_store(dmsg_control *c, const dmsg_transport_snapshot *value) {
    uint64_t words[DMSG_TRANSPORT_WORDS];
    memcpy(words, value, sizeof(words));
    atomic_fetch_add_explicit(&c->transport_sequence, 1, memory_order_seq_cst);
    for (size_t i = 0; i < DMSG_TRANSPORT_WORDS; ++i)
        atomic_store_explicit(&c->transport_words[i], words[i], memory_order_seq_cst);
    atomic_fetch_add_explicit(&c->transport_sequence, 1, memory_order_seq_cst);
}
int dmsg_control_transport_snapshot(const dmsg_control *c,
    dmsg_transport_snapshot *out, size_t out_size) {
    if (!out || out_size != sizeof(*out)) return 0;
    memset(out, 0, sizeof(*out));
    if (!c) return 0;
    for (unsigned attempt = 0; attempt < DMSG_TRANSPORT_READ_ATTEMPTS; ++attempt) {
        uint64_t before = atomic_load_explicit(&c->transport_sequence, memory_order_seq_cst);
        if (!before || (before & 1)) continue;
        uint64_t words[DMSG_TRANSPORT_WORDS];
        for (size_t i = 0; i < DMSG_TRANSPORT_WORDS; ++i)
            words[i] = atomic_load_explicit(&c->transport_words[i], memory_order_seq_cst);
        uint64_t after = atomic_load_explicit(&c->transport_sequence, memory_order_seq_cst);
        if (before == after && words[0] == DMSG_TRANSPORT_SCHEMA) {
            memcpy(out, words, sizeof(*out));
            return 1;
        }
    }
    return 0;
}
static uint64_t transport_age(uint64_t now, uint64_t time, uint64_t start) {
    return !time || time == UINT64_MAX || time < start || time > now ?
        UINT64_MAX : now - time;
}
void dmsg_transport_publish(dmsg_control *c, const picoquic_cnx_t *cnx, uint64_t now) {
    dmsg_transport_snapshot value = {0};
    if (!cnx) { transport_store(c, &value); return; }
    value.schema_version = DMSG_TRANSPORT_SCHEMA;
    value.connection_state = cnx->cnx_state;
    value.multipath = cnx->is_multipath_enabled;
    value.path_count = cnx->nb_paths > 0 ? (uint64_t)cnx->nb_paths : 0;
    value.captured_path_count = value.path_count > DMSG_TRANSPORT_MAX_PATHS ?
        DMSG_TRANSPORT_MAX_PATHS : value.path_count;
    value.now_native_us = now;
    value.packets_received = cnx->nb_packets_received;
    value.data_sent = cnx->data_sent;
    value.data_received = cnx->data_received;
    /* Both fields are seeded with start_time, not an observed event. Preserve
     * unknown even if an event shares that initial microsecond. ACK time below
     * has the same seed. Never expose the internal PN used as an ACK sentinel. */
    value.latest_receive_age_us = cnx->latest_receive_time == cnx->start_time ? UINT64_MAX :
        transport_age(now, cnx->latest_receive_time, cnx->start_time);
    value.latest_progress_age_us = cnx->latest_progress_time == cnx->start_time ? UINT64_MAX :
        transport_age(now, cnx->latest_progress_time, cnx->start_time);
    for (size_t i = 0; i < value.captured_path_count; ++i) {
        dmsg_transport_path *out = &value.paths[i];
        const picoquic_path_t *path = cnx->path[i];
        out->slot = i;
        out->oldest_pending_age_us = out->tail_pending_age_us = UINT64_MAX;
        out->highest_ack_age_us = out->last_packet_receive_age_us = UINT64_MAX;
        if (!path) continue;
        out->present = 1;
        out->validated = path->challenge_verified;
        out->demoted = path->path_is_demoted;
        out->smoothed_rtt_us = path->smoothed_rtt;
        out->base_retransmit_timer_us = path->retransmit_timer;
        out->consecutive_pto = path->nb_retransmit;
        out->bytes_in_transit = path->bytes_in_transit;
        out->cwin = path->cwin;
        out->pacing_bytes_per_second = path->pacing.rate;
        out->send_mtu = path->send_mtu;
        out->last_packet_receive_age_us = transport_age(now, path->last_packet_received_at, cnx->start_time);
        const picoquic_packet_context_t *ctx = cnx->is_multipath_enabled ? &path->pkt_ctx :
            &cnx->pkt_ctx[picoquic_packet_context_application];
        out->highest_ack_age_us = ctx->highest_acknowledged_time == cnx->start_time ? UINT64_MAX :
            transport_age(now, ctx->highest_acknowledged_time, cnx->start_time);
        const picoquic_packet_t *packet = ctx->pending_last;
        uint64_t oldest = UINT64_MAX;
        for (unsigned visited = 0; packet && visited < DMSG_TRANSPORT_SCAN_LIMIT; ++visited) {
            if (!packet->is_ack_trap) {
                if (!out->pending_count) {
                    out->tail_pending_age_us = transport_age(now, packet->send_time, cnx->start_time);
                    out->tail_ack_eliciting = !packet->is_evaluated ? DMSG_ACK_UNKNOWN :
                        packet->is_ack_eliciting ? DMSG_ACK_YES : DMSG_ACK_NO;
                }
                ++out->pending_count;
                oldest = transport_age(now, packet->send_time, cnx->start_time);
            }
            packet = packet->packet_previous;
        }
        out->pending_truncated = packet != NULL;
        if (!packet) out->oldest_pending_age_us = oldest;
    }
    transport_store(c, &value);
}
void dmsg_control_select_stream(dmsg_control *c, uint64_t request) {
    if (!(uint16_t)request || !(request >> 16)) request = 0;
    atomic_store_explicit(&c->stream_request, request, memory_order_seq_cst);
}
/* Same atomic-payload seqlock as transport, but a separate ABI/publication.
 * Private match participates in the sequence; readers compare the WHOLE request
 * before/after, so a selection change hides old data without another writer. */
static void stream_store(dmsg_control *c, uint64_t request, const dmsg_stream_snapshot *value) {
    uint64_t words[DMSG_STREAM_WORDS];
    memcpy(words, value, sizeof(words));
    atomic_fetch_add_explicit(&c->stream_sequence, 1, memory_order_seq_cst);
    atomic_store_explicit(&c->stream_match, request, memory_order_seq_cst);
    for (size_t i = 0; i < DMSG_STREAM_WORDS; ++i)
        atomic_store_explicit(&c->stream_words[i], words[i], memory_order_seq_cst);
    atomic_fetch_add_explicit(&c->stream_sequence, 1, memory_order_seq_cst);
}
int dmsg_control_stream_snapshot(const dmsg_control *c,
    dmsg_stream_snapshot *out, size_t out_size) {
    if (!out || out_size != sizeof(*out)) return 0;
    memset(out, 0, sizeof(*out));
    if (!c) return 0;
    for (unsigned attempt = 0; attempt < DMSG_STREAM_READ_ATTEMPTS; ++attempt) {
        uint64_t request = atomic_load_explicit(&c->stream_request, memory_order_seq_cst);
        if (!request) return 0;
        uint64_t before = atomic_load_explicit(&c->stream_sequence, memory_order_seq_cst);
        if (!before || (before & 1)) continue;
        uint64_t match = atomic_load_explicit(&c->stream_match, memory_order_seq_cst);
        uint64_t words[DMSG_STREAM_WORDS];
        for (size_t i = 0; i < DMSG_STREAM_WORDS; ++i)
            words[i] = atomic_load_explicit(&c->stream_words[i], memory_order_seq_cst);
        uint64_t after = atomic_load_explicit(&c->stream_sequence, memory_order_seq_cst);
        uint64_t current = atomic_load_explicit(&c->stream_request, memory_order_seq_cst);
        if (before == after && request == current && match == request &&
            words[0] == (request >> 16) && words[1] == DMSG_STREAM_SCHEMA) {
            memcpy(out, words, sizeof(*out));
            return 1;
        }
    }
    return 0;
}
static const picoquic_stream_head_t *stream_find_readonly(const picoquic_cnx_t *cnx, uint64_t id) {
    /* picoquic_find_stream/picosplay_find splay (WRITE) the owning tree. Never
     * invoke tree callbacks or helpers; following one search branch is bounded. */
    const picosplay_node_t *node = cnx->stream_tree.root;
    for (unsigned visited = 0; node && visited < DMSG_STREAM_SCAN_LIMIT; ++visited) {
        const picoquic_stream_head_t *qs = (const picoquic_stream_head_t *)
            ((const char *)node - offsetof(picoquic_stream_head_t, stream_node));
        if (qs->stream_id == id) return qs;
        node = id < qs->stream_id ? node->left : node->right;
    }
    return NULL;
}
static uint64_t stream_ack_prefix_readonly(const picoquic_stream_head_t *qs) {
    const uint64_t extent = qs->sent_offset;
    const uint64_t offset_limit = (UINT64_C(1) << 62) - 1;
    if (extent > offset_limit) return UINT64_MAX;
    /* Inclusive merged ranges, read without splaying or calling tree helpers.
     * Charge each descent before dereferencing a node; even cyclic/overdeep
     * fixtures cannot exceed 64 node entries. No upstream state is modified. */
    const picosplay_node_t *stack[DMSG_STREAM_SCAN_LIMIT];
    const picosplay_node_t *node = qs->sack_list.ack_tree.root;
    unsigned depth = 0, visited = 0;
    uint64_t prefix = 0;
    while (node || depth) {
        while (node) {
            if (visited == DMSG_STREAM_SCAN_LIMIT) return UINT64_MAX;
            ++visited;
            stack[depth++] = node;
            node = node->left;
        }
        node = stack[--depth];
        const picoquic_sack_item_t *range = (const picoquic_sack_item_t *)
            ((const char *)node - offsetof(picoquic_sack_item_t, node));
        uint64_t start = range->start_of_sack_range, end = range->end_of_sack_range;
        if (start > end || end == UINT64_MAX || end > extent ||
            (end == extent && !qs->fin_sent)) return UINT64_MAX;
        if (start > prefix) return prefix;
        uint64_t exclusive = end == extent ? extent : end + 1;
        if (exclusive > prefix) prefix = exclusive;
        if (prefix == extent) return prefix;
        node = node->right;
    }
    return prefix;
}
void dmsg_stream_publish(dmsg_control *c, const ss_conn *conn, uint64_t now) {
    dmsg_stream_snapshot value = {0};
    uint64_t request = atomic_load_explicit(&c->stream_request, memory_order_seq_cst);
    if (!request || !conn || !conn->client || !conn->ready || conn->closed || !conn->cnx)
        goto publish;
    const ss_stream *selected = NULL;
    const ss_stream *s = conn->streams;
    for (unsigned visited = 0; s && visited < DMSG_STREAM_SCAN_LIMIT; ++visited, s = s->next) {
        if (s->stage != 4 || s->fd < 0) continue;
        struct sockaddr_in peer = {0};
        socklen_t length = sizeof(peer);
        if (!getpeername(s->fd, (struct sockaddr *)&peer, &length) &&
            length == sizeof(peer) && peer.sin_family == AF_INET &&
            peer.sin_addr.s_addr == htonl(INADDR_LOOPBACK) &&
            ntohs(peer.sin_port) == (uint16_t)request) {
            selected = s;
            break;
        }
    }
    if (!selected || selected->failed || !selected->connected || selected->id == UINT64_MAX ||
        selected->out_off > selected->out_len) goto publish;
    const picoquic_stream_head_t *qs = stream_find_readonly(conn->cnx, selected->id);
    if (!qs || qs->is_closed || qs->is_discarded || qs->reset_requested || qs->reset_received ||
        qs->stop_sending_requested || qs->stop_sending_received) goto publish;
    uint64_t pending = selected->out_len - selected->out_off;
    if (pending > qs->consumed_offset || qs->sent_offset > qs->maxdata_remote ||
        qs->fin_offset < qs->consumed_offset) goto publish;
    value = (dmsg_stream_snapshot){
        .generation = request >> 16, .schema_version = DMSG_STREAM_SCHEMA, .now_native_us = now,
        .tx_submitted = qs->sent_offset, .rx_contiguous = qs->consumed_offset,
        .rx_flushed = qs->consumed_offset - pending, .rx_pending = pending,
        .stream_active = qs->is_active, .send_queue = qs->send_queue != NULL,
        .failed = selected->failed, .local_fin = selected->local_fin, .remote_fin = selected->remote_fin,
        .remote_stream_credit = qs->maxdata_remote - qs->sent_offset,
        .rx_highest = qs->fin_offset,
        .tx_ack_prefix_end = stream_ack_prefix_readonly(qs)
    };
publish:
    stream_store(c, request, &value);
}
int dmsg_bound(dmsg_control *c, int fd) {
    struct sockaddr_in a;
    socklen_t len = sizeof(a);
    if (getsockname(fd, (struct sockaddr *)&a, &len)) return -1;
    dmsg_publish(c, DMSG_LISTENING, ntohs(a.sin_port), 0);
    return 0;
}
int dmsg_native_run(dmsg_control *c, const dmsg_config *config) {
    address_t addresses[8] = {0};
    if (dmsg_cancelled(c)) { dmsg_publish(c, DMSG_STOPPED, 0, 0); return 0; }
    if (!config->resolver_count || config->resolver_count > 8) {
        dmsg_publish(c, DMSG_FAILED, 0, 2); return 2;
    }
    for (size_t i = 0; i < config->resolver_count; ++i) {
        struct sockaddr_in *v4 = (struct sockaddr_in *)&addresses[i].server_address;
        struct sockaddr_in6 *v6 = (struct sockaddr_in6 *)&addresses[i].server_address;
        if (inet_pton(AF_INET, config->resolvers[i], &v4->sin_addr) == 1) {
            v4->sin_family = AF_INET; v4->sin_port = htons(config->resolver_ports[i]);
        } else if (inet_pton(AF_INET6, config->resolvers[i], &v6->sin6_addr) == 1) {
            v6->sin6_family = AF_INET6; v6->sin6_port = htons(config->resolver_ports[i]);
        } else { dmsg_publish(c, DMSG_FAILED, 0, 2); return 2; }
    }
    /* Embedding does not consume OpenSSL configuration from process env. */
    if (!OPENSSL_init_crypto(OPENSSL_INIT_NO_LOAD_CONFIG, NULL)) {
        dmsg_publish(c, DMSG_FAILED, 0, 3); return 3;
    }
    int ret = dmsg_embedded_client(config->listen_port, addresses,
        config->resolver_count, config->domain, config->cc, false,
        config->active_keepalive_ms, config->idle_keepalive_ms,
        "127.0.0.1", NULL, false, c, config->certificate, config->certificate_len);
    /* Failure wins over a concurrent late stop. Stop is successful only if the
     * owning runtime observed cancellation before deciding to fail. */
    if (ret && atomic_load_explicit(&c->pin_failed, memory_order_acquire)) ret = 7;
    dmsg_publish(c, ret == 0 ? DMSG_STOPPED : DMSG_FAILED, 0, ret);
    return ret;
}
