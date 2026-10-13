#ifndef DMSG_SLIPSTREAM_EMBED_H
#define DMSG_SLIPSTREAM_EMBED_H
#include <stddef.h>
#include <stdint.h>
#include <stdatomic.h>

enum {
    DMSG_TRANSPORT_SCHEMA = 1,
    DMSG_TRANSPORT_MAX_PATHS = 8,
    DMSG_TRANSPORT_SCAN_LIMIT = 64,
    DMSG_TRANSPORT_READ_ATTEMPTS = 3,
    DMSG_TRANSPORT_WORDS = 155
};
enum { DMSG_ACK_UNKNOWN, DMSG_ACK_NO, DMSG_ACK_YES };

/* Private ABI v1: only uint64_t words, no pointers/identifiers/packet bytes.
 * Ages use UINT64_MAX for absent, uninitialised or invalid native timestamps.
 * Packet context is path->pkt_ctx in multipath, otherwise the connection's
 * application context, DUPLICATED in each path slot (not path-filtered).
 * Scan backwards from pending_last, visiting at most 64 nodes INCLUDING traps.
 * Count/oldest/tail exclude unsent ACK traps, but include ACK/POLL-only packets.
 * A truncated count is a lower bound; oldest age is unknown when truncated.
 * Tail is the actual last non-trap, not the last known ACK-eliciting packet.
 * Classification uses ONLY the existing is_evaluated cache; never parse bytes.
 * now_native_us is the worker's capture clock, NOT an app/sample clock. Ages
 * describe that capture, not the read; publication age/freshness is unknown. */
typedef struct {
    uint64_t slot, present, validated, demoted;
    uint64_t smoothed_rtt_us, base_retransmit_timer_us, consecutive_pto;
    uint64_t bytes_in_transit, cwin, pacing_bytes_per_second, send_mtu;
    uint64_t pending_count, pending_truncated;
    uint64_t oldest_pending_age_us, tail_pending_age_us;
    uint64_t highest_ack_age_us, last_packet_receive_age_us, tail_ack_eliciting;
} dmsg_transport_path;
typedef struct {
    uint64_t schema_version, connection_state, multipath, path_count, captured_path_count;
    uint64_t now_native_us, packets_received, data_sent, data_received;
    uint64_t latest_receive_age_us, latest_progress_age_us;
    dmsg_transport_path paths[DMSG_TRANSPORT_MAX_PATHS];
} dmsg_transport_snapshot;
_Static_assert(sizeof(dmsg_transport_path) == 18 * sizeof(uint64_t), "path ABI");
_Static_assert(sizeof(dmsg_transport_snapshot) == DMSG_TRANSPORT_WORDS * sizeof(uint64_t), "snapshot ABI");

/* Only atomics cross threads. Every upstream field stays on the existing C
 * runtime's owning worker. Status packs phase:8, port:16, error:32.
 * The transport sequence AND payload are atomic: a plain-struct seqlock would
 * be a C data race. One worker publishes; readers have a bounded retry budget. */
typedef struct {
    atomic_bool stop;
    atomic_bool pin_failed;
    atomic_uint_fast64_t snapshot;
    _Atomic uint64_t transport_sequence;
    _Atomic uint64_t transport_words[DMSG_TRANSPORT_WORDS];
} dmsg_control;
enum { DMSG_STARTING, DMSG_LISTENING, DMSG_READY, DMSG_STOPPED, DMSG_FAILED };
int dmsg_cancelled(dmsg_control *control);
void dmsg_publish(dmsg_control *control, unsigned phase, uint16_t port, int error);
int dmsg_bound(dmsg_control *control, int fd);
struct st_picoquic_cnx_t;
/* Owning worker only. NULL invalidates without accessing upstream state. */
void dmsg_transport_publish(dmsg_control *control, const struct st_picoquic_cnx_t *cnx,
    uint64_t now_native_us);
/* Copies only the atomic control, never an upstream object. 0 = unavailable;
 * out is cleared on failure (for a correctly sized output), never reused. */
int dmsg_control_transport_snapshot(const dmsg_control *control,
    dmsg_transport_snapshot *out, size_t out_size);

typedef struct {
    const char *domain;
    const char *resolvers[8];
    uint16_t resolver_ports[8];
    size_t resolver_count;
    const uint8_t *certificate;
    size_t certificate_len;
    const char *cc;
    size_t active_keepalive_ms, idle_keepalive_ms;
    uint16_t listen_port;
} dmsg_config;
#endif
