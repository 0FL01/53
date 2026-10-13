/* Disposable host TU: production C publisher/reader, real pinned Picoquic types.
 * No production test helpers. Upstream objects belong only to the test writer;
 * reader threads receive only dmsg_control. Link with section GC, without QUIC
 * libraries: the diagnostic must not call ACK/pacing/budget/time helpers. */
#include "embed.h"
#include <arpa/inet.h>
#include <assert.h>
#include <pthread.h>
#include <sched.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#if defined(__has_feature)
#if __has_feature(address_sanitizer)
#define WITH_ASAN 1
#endif
#endif
#ifdef WITH_ASAN
#include <sanitizer/asan_interface.h>
#endif

/* Count the real reader's atomic loads, and deterministically interrupt every
 * complete attempt. This instrumentation exists only in this disposable TU. */
static _Thread_local unsigned loads, sequence_loads;
static _Thread_local dmsg_control *interrupt_read;
static _Thread_local dmsg_control *interrupt_stream_read, *interrupt_selection;
static _Thread_local unsigned request_loads;
static uint64_t counted_u64(const volatile _Atomic uint64_t *p, memory_order order) {
    ++loads;
    if (interrupt_read && p == &interrupt_read->transport_sequence &&
        (++sequence_loads % 2) == 0)
        atomic_fetch_add_explicit(&interrupt_read->transport_sequence, 2, order);
    if (interrupt_stream_read && p == &interrupt_stream_read->stream_sequence &&
        (++sequence_loads % 2) == 0)
        atomic_fetch_add_explicit(&interrupt_stream_read->stream_sequence, 2, order);
    if (interrupt_selection && p == &interrupt_selection->stream_request &&
        (++request_loads % 2) == 0)
        atomic_fetch_add_explicit(&interrupt_selection->stream_request, UINT64_C(1) << 16, order);
    return atomic_load_explicit(p, order);
}
static bool counted_bool(const volatile atomic_bool *p, memory_order order) {
    ++loads;
    return atomic_load_explicit(p, order);
}
#undef atomic_load_explicit
#define atomic_load_explicit(p, o) _Generic((p), \
    atomic_bool *: counted_bool, const atomic_bool *: counted_bool, \
    default: counted_u64)((p), (o))
#include "../embed.c"

#define OFFSET(type, field, word) \
    _Static_assert(offsetof(type, field) == (word) * sizeof(uint64_t), #field " offset")
OFFSET(dmsg_transport_snapshot, schema_version, 0);
OFFSET(dmsg_transport_snapshot, connection_state, 1);
OFFSET(dmsg_transport_snapshot, multipath, 2);
OFFSET(dmsg_transport_snapshot, path_count, 3);
OFFSET(dmsg_transport_snapshot, captured_path_count, 4);
OFFSET(dmsg_transport_snapshot, now_native_us, 5);
OFFSET(dmsg_transport_snapshot, packets_received, 6);
OFFSET(dmsg_transport_snapshot, data_sent, 7);
OFFSET(dmsg_transport_snapshot, data_received, 8);
OFFSET(dmsg_transport_snapshot, latest_receive_age_us, 9);
OFFSET(dmsg_transport_snapshot, latest_progress_age_us, 10);
OFFSET(dmsg_transport_snapshot, paths, 11);
OFFSET(dmsg_transport_path, slot, 0);
OFFSET(dmsg_transport_path, present, 1);
OFFSET(dmsg_transport_path, validated, 2);
OFFSET(dmsg_transport_path, demoted, 3);
OFFSET(dmsg_transport_path, smoothed_rtt_us, 4);
OFFSET(dmsg_transport_path, base_retransmit_timer_us, 5);
OFFSET(dmsg_transport_path, consecutive_pto, 6);
OFFSET(dmsg_transport_path, bytes_in_transit, 7);
OFFSET(dmsg_transport_path, cwin, 8);
OFFSET(dmsg_transport_path, pacing_bytes_per_second, 9);
OFFSET(dmsg_transport_path, send_mtu, 10);
OFFSET(dmsg_transport_path, pending_count, 11);
OFFSET(dmsg_transport_path, pending_truncated, 12);
OFFSET(dmsg_transport_path, oldest_pending_age_us, 13);
OFFSET(dmsg_transport_path, tail_pending_age_us, 14);
OFFSET(dmsg_transport_path, highest_ack_age_us, 15);
OFFSET(dmsg_transport_path, last_packet_receive_age_us, 16);
OFFSET(dmsg_transport_path, tail_ack_eliciting, 17);
OFFSET(dmsg_stream_snapshot, generation, 0);
OFFSET(dmsg_stream_snapshot, schema_version, 1);
OFFSET(dmsg_stream_snapshot, now_native_us, 2);
OFFSET(dmsg_stream_snapshot, tx_submitted, 3);
OFFSET(dmsg_stream_snapshot, rx_contiguous, 4);
OFFSET(dmsg_stream_snapshot, rx_flushed, 5);
OFFSET(dmsg_stream_snapshot, rx_pending, 6);
OFFSET(dmsg_stream_snapshot, stream_active, 7);
OFFSET(dmsg_stream_snapshot, send_queue, 8);
OFFSET(dmsg_stream_snapshot, failed, 9);
OFFSET(dmsg_stream_snapshot, local_fin, 10);
OFFSET(dmsg_stream_snapshot, remote_fin, 11);
OFFSET(dmsg_stream_snapshot, remote_stream_credit, 12);
OFFSET(dmsg_stream_snapshot, rx_highest, 13);
OFFSET(dmsg_stream_snapshot, tx_ack_prefix_end, 14);

static dmsg_transport_snapshot read_snapshot(dmsg_control *c) {
    dmsg_transport_snapshot out;
    assert(dmsg_control_transport_snapshot(c, &out, sizeof(out)) == 1);
    return out;
}
static void unavailable(dmsg_control *c) {
    dmsg_transport_snapshot out;
    memset(&out, 0xa5, sizeof(out));
    assert(dmsg_control_transport_snapshot(c, &out, sizeof(out)) == 0);
    const dmsg_transport_snapshot zero = {0};
    assert(memcmp(&out, &zero, sizeof(out)) == 0);
}

static void availability_and_read_bound(void) {
    dmsg_control *c = dmsg_control_new(); assert(c);
    assert(atomic_is_lock_free(&c->transport_sequence));
    assert(atomic_is_lock_free(&c->transport_words[0]));
    loads = 0; unavailable(c); assert(loads == DMSG_TRANSPORT_READ_ATTEMPTS);
    unavailable(NULL);
    dmsg_transport_snapshot out;
    memset(&out, 0xa5, sizeof(out));
    const dmsg_transport_snapshot untouched = out;
    assert(!dmsg_control_transport_snapshot(c, &out, sizeof(out) - 8));
    assert(memcmp(&out, &untouched, sizeof(out)) == 0);
    assert(!dmsg_control_transport_snapshot(c, NULL, sizeof(out)));
    picoquic_cnx_t cnx = {0};
    dmsg_transport_publish(c, &cnx, 123);
    loads = 0; out = read_snapshot(c); assert(loads == DMSG_TRANSPORT_WORDS + 2);
    assert(out.schema_version == 1 && out.now_native_us == 123);
    assert(out.path_count == 0 && out.captured_path_count == 0);
    cnx.nb_paths = -1;
    dmsg_transport_publish(c, &cnx, 123); out = read_snapshot(c);
    assert(out.path_count == 0 && out.captured_path_count == 0);
    atomic_fetch_add_explicit(&c->transport_sequence, 1, memory_order_seq_cst);
    loads = 0; unavailable(c); assert(loads == 3); /* Writer paused mid-publication. */
    atomic_fetch_add_explicit(&c->transport_sequence, 1, memory_order_seq_cst);
    interrupt_read = c; sequence_loads = loads = 0;
    unavailable(c);
    assert(loads == 3 * (DMSG_TRANSPORT_WORDS + 2) && sequence_loads == 6);
    interrupt_read = NULL;
    out.schema_version = 2; transport_store(c, &out);
    loads = 0; unavailable(c); assert(loads == 3 * (DMSG_TRANSPORT_WORDS + 2));
    dmsg_transport_publish(c, &cnx, 124);
    dmsg_publish(c, DMSG_STOPPED, 0, 0); unavailable(c);
    dmsg_transport_publish(c, &cnx, 125);
    dmsg_publish(c, DMSG_FAILED, 0, 7); unavailable(c);
    dmsg_transport_publish(c, NULL, 0); unavailable(c);
    dmsg_control_free(c);
}

/* RW object with an adjacent unreadable page. Used also to put exactly eight
 * path pointers at the end of accessible memory, and guard the 65th node. */
typedef struct { void *base, *object, *guard; size_t length, accessible; } region;
static region allocate_region(size_t size, bool at_end) {
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    size_t accessible = ((size + page - 1) / page) * page;
    void *base = mmap(NULL, accessible + page, PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(base != MAP_FAILED);
    void *guard = (char *)base + accessible;
    assert(mprotect(guard, page, PROT_NONE) == 0);
    return (region){base, at_end ? (char *)guard - size : base,
        guard, accessible + page, accessible};
}
static void readonly(region r) { assert(mprotect(r.base, r.accessible, PROT_READ) == 0); }
static void release(region r) { assert(munmap(r.base, r.length) == 0); }

static void ages_contexts_traps_and_scan_bounds(void) {
    dmsg_control *c = dmsg_control_new(); assert(c);
    picoquic_cnx_t cnx = {0}; picoquic_path_t paths[2] = {0};
    picoquic_path_t *slots[] = {&paths[0], &paths[1]};
    picoquic_packet_t *packets = calloc(65, sizeof(*packets)); assert(packets);
    cnx.path = slots; cnx.nb_paths = 2; cnx.start_time = 100;
    cnx.latest_receive_time = cnx.latest_progress_time = 100;
    picoquic_packet_context_t *app = &cnx.pkt_ctx[picoquic_packet_context_application];
    app->highest_acknowledged_time = 100;
    dmsg_transport_publish(c, &cnx, 1000);
    dmsg_transport_snapshot out = read_snapshot(c);
    assert(out.latest_receive_age_us == UINT64_MAX && out.latest_progress_age_us == UINT64_MAX);
    assert(out.paths[0].highest_ack_age_us == UINT64_MAX);
    assert(out.paths[0].last_packet_receive_age_us == UINT64_MAX);
    assert(out.paths[0].pending_count == 0 && !out.paths[0].pending_truncated);
    assert(out.paths[0].oldest_pending_age_us == UINT64_MAX);
    assert(out.paths[0].tail_pending_age_us == UINT64_MAX && out.paths[0].tail_ack_eliciting == DMSG_ACK_UNKNOWN);
    for (size_t i = 0; i < 65; ++i) {
        packets[i].send_time = 200 + i;
        packets[i].packet_previous = i ? &packets[i - 1] : NULL;
        packets[i].is_evaluated = packets[i].is_ack_eliciting = 1;
        packets[i].send_path = &paths[i % 2]; /* Legacy context is NOT path-filtered. */
    }
    app->pending_last = &packets[2];
    packets[2].is_ack_trap = 1; /* Unsent trap at actual pending_last. */
    packets[1].is_ack_eliciting = 0; /* ACK/POLL-only tail still counts. */
    app->highest_acknowledged_time = 800;
    cnx.latest_receive_time = 1000; cnx.latest_progress_time = 700;
    paths[0].last_packet_received_at = 900;
    paths[0].pkt_ctx.pending_last = &packets[0];
    paths[1].pkt_ctx.pending_last = &packets[3];
    paths[0].pkt_ctx.highest_acknowledged_time = 600;
    paths[1].pkt_ctx.highest_acknowledged_time = 500;
    dmsg_transport_publish(c, &cnx, 1000); out = read_snapshot(c);
    assert(out.latest_receive_age_us == 0 && out.latest_progress_age_us == 300);
    for (unsigned i = 0; i < 2; ++i) {
        assert(out.paths[i].pending_count == 2 && !out.paths[i].pending_truncated);
        assert(out.paths[i].oldest_pending_age_us == 800 && out.paths[i].tail_pending_age_us == 799);
        assert(out.paths[i].highest_ack_age_us == 200 && out.paths[i].tail_ack_eliciting == DMSG_ACK_NO);
    }
    assert(out.paths[0].last_packet_receive_age_us == 100);
    cnx.is_multipath_enabled = 1;
    dmsg_transport_publish(c, &cnx, 1000); out = read_snapshot(c);
    assert(out.paths[0].pending_count == 1 && out.paths[1].pending_count == 3);
    assert(out.paths[0].highest_ack_age_us == 400 && out.paths[1].highest_ack_age_us == 500);
    assert(out.paths[0].tail_ack_eliciting == DMSG_ACK_YES);
    cnx.is_multipath_enabled = 0;
    packets[1].is_evaluated = 0; packets[1].is_ack_eliciting = 1;
    dmsg_transport_publish(c, &cnx, 1000); out = read_snapshot(c);
    assert(out.paths[0].tail_ack_eliciting == DMSG_ACK_UNKNOWN);
    packets[2].is_ack_trap = 0;
    for (unsigned count = 63; count <= 65; ++count) {
        app->pending_last = &packets[count - 1];
        dmsg_transport_publish(c, &cnx, 1000); out = read_snapshot(c);
        assert(out.paths[0].pending_count == (count > 64 ? 64 : count));
        assert(out.paths[0].pending_truncated == (count > 64));
        assert(out.paths[0].oldest_pending_age_us == (count > 64 ? UINT64_MAX : 800));
        assert(out.paths[0].tail_pending_age_us == 1000 - (200 + count - 1));
    }
    region guard = allocate_region(8, false);
    packets[0].packet_previous = guard.guard;
    app->pending_last = &packets[63];
    for (unsigned i = 0; i < 64; ++i) packets[i].is_ack_trap = 1;
    dmsg_transport_publish(c, &cnx, 1000); out = read_snapshot(c);
    assert(out.paths[0].pending_truncated && out.paths[0].pending_count == 0);
    assert(out.paths[0].tail_ack_eliciting == DMSG_ACK_UNKNOWN);
    assert(out.paths[0].tail_pending_age_us == UINT64_MAX && out.paths[0].oldest_pending_age_us == UINT64_MAX);
    packets[0].packet_previous = NULL;
    dmsg_transport_publish(c, &cnx, 1000); out = read_snapshot(c);
    assert(!out.paths[0].pending_truncated && out.paths[0].pending_count == 0);
    packets[0].is_ack_trap = 0; app->pending_last = &packets[0];
    const uint64_t invalid[] = {0, UINT64_MAX, 99, 1001};
    for (unsigned i = 0; i < sizeof(invalid) / sizeof(invalid[0]); ++i) {
        cnx.latest_receive_time = cnx.latest_progress_time = invalid[i];
        app->highest_acknowledged_time = paths[0].last_packet_received_at = packets[0].send_time = invalid[i];
        dmsg_transport_publish(c, &cnx, 1000); out = read_snapshot(c);
        assert(out.latest_receive_age_us == UINT64_MAX && out.latest_progress_age_us == UINT64_MAX);
        assert(out.paths[0].highest_ack_age_us == UINT64_MAX && out.paths[0].last_packet_receive_age_us == UINT64_MAX);
        assert(out.paths[0].tail_pending_age_us == UINT64_MAX && out.paths[0].oldest_pending_age_us == UINT64_MAX);
    }
    packets[0].send_time = app->highest_acknowledged_time = paths[0].last_packet_received_at = 1000;
    cnx.latest_receive_time = cnx.latest_progress_time = 1000;
    dmsg_transport_publish(c, &cnx, 1000); out = read_snapshot(c);
    assert(out.latest_receive_age_us == 0 && out.latest_progress_age_us == 0);
    assert(out.paths[0].highest_ack_age_us == 0 && out.paths[0].last_packet_receive_age_us == 0);
    assert(out.paths[0].tail_pending_age_us == 0 && out.paths[0].oldest_pending_age_us == 0);
    free(packets); release(guard); dmsg_control_free(c);
}

static void readonly_upstream_and_path_bound(void) {
    region cr = allocate_region(sizeof(picoquic_cnx_t), false);
    region pr = allocate_region(sizeof(picoquic_path_t), false);
    region ar = allocate_region(8 * sizeof(picoquic_path_t *), true);
    region pk = allocate_region(sizeof(picoquic_packet_t), false);
    picoquic_cnx_t *cnx = cr.object; picoquic_path_t *path = pr.object;
    picoquic_packet_t *packet = pk.object; picoquic_path_t **slots = ar.object;
    cnx->nb_paths = 12; cnx->path = slots; cnx->start_time = 100;
    cnx->quic = cr.guard; /* A helper reading upstream QUIC would fault. */
    for (unsigned i = 0; i < 8; ++i) slots[i] = i == 3 ? NULL : path;
    cnx->pkt_ctx[picoquic_packet_context_application].pending_last = packet;
    /* No access to handshake/initial contexts, addresses, CIDs, PN or bytes. */
    cnx->pkt_ctx[picoquic_packet_context_initial].pending_last = pk.guard;
    cnx->pkt_ctx[picoquic_packet_context_handshake].pending_last = pk.guard;
    path->unique_path_id = UINT64_C(0xdeadbeef12345678);
    path->challenge_verified = path->path_is_demoted = 1;
    path->smoothed_rtt = 726224; path->retransmit_timer = 721022;
    path->nb_retransmit = 9; path->bytes_in_transit = 81; path->cwin = 82;
    path->pacing.rate = 83; path->send_mtu = 84;
    packet->send_time = 500; packet->sequence_number = UINT64_MAX;
    packet->is_evaluated = 0; packet->is_ack_eliciting = 1;
    packet->length = SIZE_MAX; packet->offset = 0;
    memset(packet->bytes, 0xff, sizeof(packet->bytes));
#ifdef WITH_ASAN
    __asan_poison_memory_region(packet->bytes, sizeof(packet->bytes));
#endif
    readonly(cr); readonly(pr); readonly(ar); readonly(pk);
    dmsg_control *c = dmsg_control_new(); assert(c);
    dmsg_transport_publish(c, cnx, 1000);
    dmsg_transport_snapshot out = read_snapshot(c);
    assert(out.path_count == 12 && out.captured_path_count == 8);
    for (unsigned i = 0; i < 8; ++i) {
        assert(out.paths[i].slot == i && out.paths[i].present == (i != 3));
        if (i == 3) {
            assert(out.paths[i].oldest_pending_age_us == UINT64_MAX);
            continue;
        }
        assert(out.paths[i].validated && out.paths[i].demoted);
        assert(out.paths[i].smoothed_rtt_us == 726224 && out.paths[i].base_retransmit_timer_us == 721022);
        assert(out.paths[i].consecutive_pto == 9 && out.paths[i].bytes_in_transit == 81);
        assert(out.paths[i].cwin == 82 && out.paths[i].pacing_bytes_per_second == 83 && out.paths[i].send_mtu == 84);
        assert(out.paths[i].pending_count == 1 && out.paths[i].tail_pending_age_us == 500);
        assert(out.paths[i].tail_ack_eliciting == DMSG_ACK_UNKNOWN);
    }
#ifdef WITH_ASAN
    __asan_unpoison_memory_region(packet->bytes, sizeof(packet->bytes));
#endif
    release(cr); release(pr); release(ar); release(pk);
    /* Reader still works after all upstream structures cease to exist. */
    const dmsg_transport_snapshot copy = read_snapshot(c);
    assert(memcmp(&out, &copy, sizeof(out)) == 0);
    dmsg_control_free(c);
}

typedef struct {
    picoquic_cnx_t cnx;
    picoquic_path_t paths[8];
    picoquic_path_t *slots[8];
    picoquic_packet_t packets[8][2];
} generation_fixture;
static void generation(generation_fixture *f, uint64_t g) {
    f->cnx.cnx_state = picoquic_state_ready;
    f->cnx.is_multipath_enabled = 1;
    f->cnx.nb_paths = 8; f->cnx.path = f->slots; f->cnx.start_time = 1;
    f->cnx.latest_receive_time = 2; f->cnx.latest_progress_time = 3;
    f->cnx.nb_packets_received = g + 1; f->cnx.data_sent = g + 2; f->cnx.data_received = g + 3;
    for (unsigned i = 0; i < 8; ++i) {
        picoquic_path_t *p = f->slots[i] = &f->paths[i];
        p->challenge_verified = (g + i) % 2 == 0; p->path_is_demoted = (g + i) % 2 != 0;
        p->smoothed_rtt = g + 100 + i; p->retransmit_timer = g + 200 + i;
        p->nb_retransmit = g + 300 + i; p->bytes_in_transit = g + 400 + i;
        p->cwin = g + 500 + i; p->pacing.rate = g + 600 + i; p->send_mtu = g + 700 + i;
        p->pkt_ctx.highest_acknowledged_time = 10 + i; p->last_packet_received_at = 20 + i;
        f->packets[i][0].send_time = 3 + i; f->packets[i][1].send_time = 4 + i;
        f->packets[i][1].packet_previous = &f->packets[i][0];
        f->packets[i][1].is_evaluated = 1;
        f->packets[i][1].is_ack_eliciting = (g + i) % 2 == 0;
        p->pkt_ctx.pending_last = &f->packets[i][1];
    }
}
static dmsg_transport_snapshot expected_generation(uint64_t g) {
    dmsg_transport_snapshot out = {
        .schema_version = 1, .connection_state = picoquic_state_ready, .multipath = 1,
        .path_count = 8, .captured_path_count = 8, .now_native_us = 1000 + g,
        .packets_received = g + 1, .data_sent = g + 2, .data_received = g + 3,
        .latest_receive_age_us = 998 + g, .latest_progress_age_us = 997 + g
    };
    for (unsigned i = 0; i < 8; ++i) out.paths[i] = (dmsg_transport_path){
        .slot = i, .present = 1, .validated = (g + i) % 2 == 0, .demoted = (g + i) % 2 != 0,
        .smoothed_rtt_us = g + 100 + i, .base_retransmit_timer_us = g + 200 + i,
        .consecutive_pto = g + 300 + i, .bytes_in_transit = g + 400 + i,
        .cwin = g + 500 + i, .pacing_bytes_per_second = g + 600 + i, .send_mtu = g + 700 + i,
        .pending_count = 2, .oldest_pending_age_us = 997 + g - i,
        .tail_pending_age_us = 996 + g - i, .highest_ack_age_us = 990 + g - i,
        .last_packet_receive_age_us = 980 + g - i,
        .tail_ack_eliciting = (g + i) % 2 == 0 ? DMSG_ACK_YES : DMSG_ACK_NO
    };
    return out;
}

enum { WRITES = 20000, READERS = 4 };
typedef struct {
    dmsg_control *control;
    pthread_barrier_t barrier;
    atomic_bool done;
    _Atomic uint64_t accepted, unavailable, intermediate;
} concurrent_fixture;
static void *writer(void *arg) {
    concurrent_fixture *f = arg;
    generation_fixture *source = calloc(1, sizeof(*source)); assert(source);
    pthread_barrier_wait(&f->barrier);
    for (uint64_t g = 1; g <= WRITES; ++g) {
        generation(source, g);
        dmsg_transport_publish(f->control, &source->cnx, 1000 + g);
        if (g % 32 == 0) sched_yield();
        /* Ensure at least one accepted non-final generation, so success cannot
         * be satisfied solely by reading after the owner finishes publishing. */
        if (g == WRITES / 2)
            while (!atomic_load_explicit(&f->intermediate, memory_order_acquire)) sched_yield();
    }
    free(source);
    atomic_store_explicit(&f->done, true, memory_order_release);
    return NULL;
}
static void *reader(void *arg) {
    concurrent_fixture *f = arg;
    pthread_barrier_wait(&f->barrier);
    do {
        dmsg_transport_snapshot out;
        loads = 0;
        int available = dmsg_control_transport_snapshot(f->control, &out, sizeof(out));
        assert(loads <= 3 * (DMSG_TRANSPORT_WORDS + 2));
        if (available) {
            assert(out.now_native_us >= 1001 && out.now_native_us <= 1000 + WRITES);
            const dmsg_transport_snapshot expected = expected_generation(out.now_native_us - 1000);
            assert(memcmp(&out, &expected, sizeof(out)) == 0); /* All 155 words coherent. */
            atomic_fetch_add_explicit(&f->accepted, 1, memory_order_relaxed);
            if (out.now_native_us > 1001 && out.now_native_us < 1000 + WRITES)
                atomic_fetch_add_explicit(&f->intermediate, 1, memory_order_release);
        } else {
            const dmsg_transport_snapshot zero = {0};
            assert(memcmp(&out, &zero, sizeof(out)) == 0);
            atomic_fetch_add_explicit(&f->unavailable, 1, memory_order_relaxed);
        }
    } while (!atomic_load_explicit(&f->done, memory_order_acquire));
    return NULL;
}
static void concurrent_publication(void) {
    concurrent_fixture f = {.control = dmsg_control_new()}; assert(f.control);
    atomic_init(&f.done, false); atomic_init(&f.accepted, 0); atomic_init(&f.unavailable, 0);
    atomic_init(&f.intermediate, 0);
    assert(pthread_barrier_init(&f.barrier, NULL, READERS + 1) == 0);
    pthread_t owner, readers[READERS];
    assert(pthread_create(&owner, NULL, writer, &f) == 0);
    for (unsigned i = 0; i < READERS; ++i) assert(pthread_create(&readers[i], NULL, reader, &f) == 0);
    assert(pthread_join(owner, NULL) == 0);
    for (unsigned i = 0; i < READERS; ++i) assert(pthread_join(readers[i], NULL) == 0);
    assert(atomic_load(&f.accepted) > 0 && atomic_load(&f.unavailable) > 0);
    assert(atomic_load(&f.intermediate) > 0);
    printf("C atomic payload: %u publications, %u readers, %llu coherent reads (%llu non-final), %llu unavailable\n",
        WRITES, READERS, (unsigned long long)atomic_load(&f.accepted),
        (unsigned long long)atomic_load(&f.intermediate),
        (unsigned long long)atomic_load(&f.unavailable));
    assert(pthread_barrier_destroy(&f.barrier) == 0);
    dmsg_control_free(f.control);
}

/* Two actual accepted TCP fds: getpeername must match the Rust-side LOCAL
 * ephemeral port, not the listener or the accepted fd's local port. */
typedef struct {
    ss_conn conn;
    picoquic_cnx_t cnx;
    ss_stream streams[2];
    picoquic_stream_head_t quic_streams[2];
    int peers[2];
    uint16_t ports[2], listener_port;
} stream_fixture;
static void stream_fixture_init(stream_fixture *f) {
    memset(f, 0, sizeof(*f));
    int listener = socket(AF_INET, SOCK_STREAM, 0); assert(listener >= 0);
    struct sockaddr_in address = {.sin_family = AF_INET, .sin_addr.s_addr = htonl(INADDR_LOOPBACK)};
    assert(bind(listener, (struct sockaddr *)&address, sizeof(address)) == 0);
    assert(listen(listener, 2) == 0);
    socklen_t size = sizeof(address);
    assert(getsockname(listener, (struct sockaddr *)&address, &size) == 0);
    f->listener_port = ntohs(address.sin_port);
    f->conn = (ss_conn){.client = true, .ready = true, .count = 2,
        .cnx = &f->cnx, .streams = &f->streams[0]};
    for (unsigned i = 0; i < 2; ++i) {
        f->peers[i] = socket(AF_INET, SOCK_STREAM, 0); assert(f->peers[i] >= 0);
        assert(connect(f->peers[i], (struct sockaddr *)&address, sizeof(address)) == 0);
        f->streams[i] = (ss_stream){.id = 4 + 4 * i, .fd = accept(listener, NULL, NULL),
            .connected = true, .stage = 4, .out_len = 100 + i, .out_off = 30 + i,
            .local_fin = i == 1, .remote_fin = i == 0};
        assert(f->streams[i].fd >= 0);
        struct sockaddr_in local = {0}; size = sizeof(local);
        assert(getsockname(f->peers[i], (struct sockaddr *)&local, &size) == 0);
        f->ports[i] = ntohs(local.sin_port);
        f->quic_streams[i] = (picoquic_stream_head_t){.stream_id = f->streams[i].id,
            .sent_offset = 123 + i, .consumed_offset = 456 + i, .maxdata_remote = 1000 + i,
            .fin_offset = 600 + i,
            .is_active = i == 0};
    }
    close(listener);
    assert(f->ports[0] != f->ports[1] && f->ports[0] != f->listener_port && f->ports[1] != f->listener_port);
    f->streams[0].next = &f->streams[1];
    f->cnx.stream_tree.root = &f->quic_streams[1].stream_node;
    f->quic_streams[1].stream_node.left = &f->quic_streams[0].stream_node;
    f->quic_streams[0].stream_node.parent = &f->quic_streams[1].stream_node;
    /* Presence only; never read queue nodes, output/setup or tree callbacks. */
    f->quic_streams[1].send_queue = (void *)(uintptr_t)1;
    f->streams[0].out = f->streams[1].out = (void *)(uintptr_t)1;
}
static void stream_fixture_close(stream_fixture *f) {
    for (unsigned i = 0; i < 2; ++i) { close(f->peers[i]); close(f->streams[i].fd); }
}
static void select_lane(dmsg_control *c, const stream_fixture *f, unsigned lane, uint64_t generation) {
    dmsg_control_select_stream(c, (generation << 16) | f->ports[lane]);
}
static dmsg_stream_snapshot stream_read(dmsg_control *c) {
    dmsg_stream_snapshot out;
    assert(dmsg_control_stream_snapshot(c, &out, sizeof(out)) == 1);
    return out;
}
static void stream_unavailable(dmsg_control *c) {
    dmsg_stream_snapshot out; memset(&out, 0xa5, sizeof(out));
    assert(dmsg_control_stream_snapshot(c, &out, sizeof(out)) == 0);
    const dmsg_stream_snapshot zero = {0}; assert(memcmp(&out, &zero, sizeof(out)) == 0);
}
static dmsg_stream_snapshot stream_expected(unsigned lane, uint64_t generation, uint64_t now) {
    return (dmsg_stream_snapshot){.generation = generation, .schema_version = DMSG_STREAM_SCHEMA, .now_native_us = now,
        .tx_submitted = 123 + lane, .rx_contiguous = 456 + lane, .rx_pending = 70,
        .rx_flushed = 386 + lane, .stream_active = lane == 0, .send_queue = lane == 1,
        .local_fin = lane == 1, .remote_fin = lane == 0, .remote_stream_credit = 877,
        .rx_highest = 600 + lane};
}
static void stream_selection_generation_retirement_and_read_bound(void) {
    stream_fixture f; stream_fixture_init(&f);
    dmsg_control *c = dmsg_control_new(); assert(c);
    assert(atomic_is_lock_free(&c->stream_request) && atomic_is_lock_free(&c->stream_words[0]));
    assert(atomic_is_lock_free(&c->stream_sequence) && atomic_is_lock_free(&c->stream_match));
    loads = 0; stream_unavailable(c); assert(loads == 1); stream_unavailable(NULL);
    select_lane(c, &f, 0, 42); stream_unavailable(c);
    dmsg_stream_publish(c, &f.conn, 1042);
    loads = 0; dmsg_stream_snapshot out = stream_read(c); assert(loads == DMSG_STREAM_WORDS + 5);
    const dmsg_stream_snapshot expected = stream_expected(0, 42, 1042);
    assert(memcmp(&out, &expected, sizeof(out)) == 0);
    select_lane(c, &f, 1, 43); stream_unavailable(c); /* No former lane before publication. */
    dmsg_stream_publish(c, &f.conn, 1043); out = stream_read(c);
    const dmsg_stream_snapshot second = stream_expected(1, 43, 1043);
    assert(memcmp(&out, &second, sizeof(out)) == 0);
    out.schema_version = 2; /* Reject the former 14-word schema, not a zero sample. */
    stream_store(c, (UINT64_C(43) << 16) | f.ports[1], &out);
    loads = 0; stream_unavailable(c); assert(loads == 3 * (DMSG_STREAM_WORDS + 5));
    dmsg_stream_publish(c, &f.conn, 1043);
    select_lane(c, &f, 0, 43); stream_unavailable(c); /* Whole key, even if generation misused. */
    select_lane(c, &f, 1, 44); stream_unavailable(c); /* Same port, fresh lane lifetime. */
    /* An already-started old worker publication finishing AFTER selection
     * changed is still unavailable; no selector ever becomes a payload writer. */
    stream_store(c, (UINT64_C(43) << 16) | f.ports[1], &second); stream_unavailable(c);
    dmsg_stream_publish(c, &f.conn, 1044);
    interrupt_stream_read = c; sequence_loads = loads = 0; stream_unavailable(c);
    assert(loads == 3 * (DMSG_STREAM_WORDS + 5)); interrupt_stream_read = NULL;
    interrupt_selection = c; request_loads = loads = 0; stream_unavailable(c);
    assert(request_loads == 6 && loads == 3 * (DMSG_STREAM_WORDS + 5)); interrupt_selection = NULL;
    select_lane(c, &f, 1, 45); dmsg_stream_publish(c, &f.conn, 1045);
    atomic_fetch_add(&c->stream_sequence, 1); loads = 0; stream_unavailable(c); assert(loads == 6);
    atomic_fetch_add(&c->stream_sequence, 1);
    memset(&out, 0xa5, sizeof(out)); const dmsg_stream_snapshot untouched = out;
    assert(!dmsg_control_stream_snapshot(c, &out, sizeof(out) - 8));
    assert(memcmp(&out, &untouched, sizeof(out)) == 0);
    assert(!dmsg_control_stream_snapshot(c, NULL, sizeof(out)));
    f.streams[1].failed = true; dmsg_stream_publish(c, &f.conn, 1046); stream_unavailable(c);
    f.streams[1].failed = false;
    f.streams[1].stage = 3; dmsg_stream_publish(c, &f.conn, 1047); stream_unavailable(c);
    f.streams[1].stage = 4;
    f.streams[1].out_off = f.streams[1].out_len + 1;
    dmsg_stream_publish(c, &f.conn, 1048); stream_unavailable(c);
    f.streams[1].out_off = 0; f.streams[1].out_len = f.quic_streams[1].consumed_offset + 1;
    dmsg_stream_publish(c, &f.conn, 1049); stream_unavailable(c);
    f.streams[1].out_len = f.streams[1].out_off = 0;
    f.quic_streams[1].sent_offset = 1002; dmsg_stream_publish(c, &f.conn, 1050); stream_unavailable(c);
    f.quic_streams[1].sent_offset = 124;
    f.quic_streams[1].reset_received = 1; dmsg_stream_publish(c, &f.conn, 1051); stream_unavailable(c);
    f.quic_streams[1].reset_received = 0;
    f.quic_streams[1].fin_offset = f.quic_streams[1].consumed_offset - 1;
    dmsg_stream_publish(c, &f.conn, 1051); stream_unavailable(c);
    f.quic_streams[1].fin_offset = 601;
    f.cnx.stream_tree.root = NULL; dmsg_stream_publish(c, &f.conn, 1052); stream_unavailable(c);
    f.cnx.stream_tree.root = &f.quic_streams[1].stream_node;
    f.streams[0].next = NULL; dmsg_stream_publish(c, &f.conn, 1053); stream_unavailable(c); /* Retirement. */
    f.streams[0].next = &f.streams[1];
    dmsg_control_select_stream(c, (UINT64_C(45) << 16) | f.listener_port);
    dmsg_stream_publish(c, &f.conn, 1054); stream_unavailable(c); /* Not the listener port. */
    select_lane(c, &f, 1, (UINT64_C(1) << 48) - 1); dmsg_stream_publish(c, &f.conn, 1055);
    assert(stream_read(c).generation == (UINT64_C(1) << 48) - 1);
    dmsg_control_select_stream(c, 0); stream_unavailable(c);
    dmsg_control_select_stream(c, f.ports[1]); stream_unavailable(c);
    dmsg_control_select_stream(c, UINT64_C(1) << 16); stream_unavailable(c);
    select_lane(c, &f, 1, 46); dmsg_stream_publish(c, &f.conn, 1056);
    dmsg_publish(c, DMSG_STOPPED, 0, 0); stream_unavailable(c);
    dmsg_stream_publish(c, &f.conn, 1057); dmsg_publish(c, DMSG_FAILED, 0, 1); stream_unavailable(c);
    dmsg_stream_publish(c, NULL, 0); stream_unavailable(c);
    f.quic_streams[1].sent_offset = f.quic_streams[1].consumed_offset = f.quic_streams[1].maxdata_remote = 0;
    f.quic_streams[1].fin_offset = 0;
    f.quic_streams[1].is_active = 0; f.quic_streams[1].send_queue = NULL;
    f.streams[1].local_fin = f.streams[1].remote_fin = false;
    select_lane(c, &f, 1, 47); dmsg_stream_publish(c, &f.conn, 1058); out = stream_read(c);
    const dmsg_stream_snapshot known_zero = {.generation = 47, .schema_version = DMSG_STREAM_SCHEMA, .now_native_us = 1058};
    assert(memcmp(&out, &known_zero, sizeof(out)) == 0); /* Known zero != unavailable. */
    stream_fixture_close(&f); dmsg_control_free(c);
}
static void stream_readonly_and_scan_bounds(void) {
    stream_fixture f; stream_fixture_init(&f);
    region cr = allocate_region(sizeof(ss_conn), false);
    region qr = allocate_region(sizeof(picoquic_cnx_t), false);
    region sr = allocate_region(64 * sizeof(ss_stream), true);
    region tr = allocate_region(64 * sizeof(picoquic_stream_head_t), true);
    ss_conn *conn = cr.object; picoquic_cnx_t *cnx = qr.object;
    ss_stream *streams = sr.object; picoquic_stream_head_t *qs = tr.object;
    *conn = f.conn; conn->cnx = cnx; conn->streams = streams; conn->count = 999;
    cnx->quic = qr.guard;
    for (unsigned i = 0; i < 64; ++i) {
        streams[i] = (ss_stream){.next = i < 63 ? &streams[i + 1] : sr.guard, .stage = 0};
        qs[i].stream_id = 2 * i;
        qs[i].stream_node.right = i < 63 ? &qs[i + 1].stream_node : tr.guard;
    }
    cnx->stream_tree.root = &qs[0].stream_node;
    readonly(cr); readonly(qr); readonly(sr); readonly(tr);
    dmsg_control *c = dmsg_control_new(); assert(c); select_lane(c, &f, 0, 99);
    dmsg_stream_publish(c, conn, 1099); stream_unavailable(c); /* Never reads list node 65. */
    assert(mprotect(sr.base, sr.accessible, PROT_READ | PROT_WRITE) == 0);
    streams[63] = f.streams[0]; streams[63].id = 128; streams[63].next = sr.guard;
    readonly(sr);
    dmsg_stream_publish(c, conn, 1100); stream_unavailable(c); /* Never reads tree node 65. */
    assert(mprotect(sr.base, sr.accessible, PROT_READ | PROT_WRITE) == 0);
    streams[63].id = 126; readonly(sr);
    assert(mprotect(tr.base, tr.accessible, PROT_READ | PROT_WRITE) == 0);
    picosplay_node_t node = qs[63].stream_node;
    qs[63] = f.quic_streams[0]; qs[63].stream_id = 126; qs[63].stream_node = node;
    readonly(tr);
    dmsg_stream_publish(c, conn, 1101);
    const dmsg_stream_snapshot out = stream_read(c), expected = stream_expected(0, 99, 1101);
    assert(memcmp(&out, &expected, sizeof(out)) == 0); /* Exactly 64 list/tree visits; no splay writes. */
    release(cr); release(qr); release(sr); release(tr);
    assert(memcmp(&out, &(dmsg_stream_snapshot){0}, sizeof(out)) != 0);
    const dmsg_stream_snapshot copy = stream_read(c); assert(memcmp(&out, &copy, sizeof(out)) == 0);
    stream_fixture_close(&f); dmsg_control_free(c);
}

/* Real SACK nodes, assembled by hand: sacks.c orders inclusive ranges by start
 * and merges touching/overlapping ACKs. FIN occupies sent_offset itself. Never
 * initialize/find/update/iterate the tree through upstream splay/SACK helpers. */
static int64_t forbidden_sack_compare(void *left, void *right) {
    (void)left; (void)right; abort();
}
static picosplay_node_t *forbidden_sack_create(void *value) { (void)value; abort(); }
static void forbidden_sack_delete(void *tree, picosplay_node_t *node) {
    (void)tree; (void)node; abort();
}
static void *forbidden_sack_value(picosplay_node_t *node) { (void)node; abort(); }
static unsigned sack_cases;
static void stream_ack_case(stream_fixture *f, dmsg_control *c, region qr, region ar,
    uint64_t expected_prefix) {
    picoquic_stream_head_t *qs = qr.object;
    qs->maxdata_remote = qs->sent_offset;
    const picoquic_stream_head_t before = *qs;
    const size_t size = DMSG_STREAM_SCAN_LIMIT * sizeof(picoquic_sack_item_t);
    void *before_ranges = malloc(size); assert(before_ranges);
    memcpy(before_ranges, ar.object, size);
    readonly(qr); readonly(ar);
    assert(stream_ack_prefix_readonly(qs) == expected_prefix);
    dmsg_stream_publish(c, &f->conn, 1200);
    const dmsg_stream_snapshot out = stream_read(c);
    dmsg_stream_snapshot expected = stream_expected(0, 100, 1200);
    expected.tx_submitted = qs->sent_offset; expected.remote_stream_credit = 0;
    expected.tx_ack_prefix_end = expected_prefix;
    assert(memcmp(&out, &expected, sizeof(out)) == 0); /* Unknown scalar != unavailable DTO. */
    assert(memcmp(qs, &before, sizeof(before)) == 0);
    assert(memcmp(ar.object, before_ranges, size) == 0);
    free(before_ranges); ++sack_cases;
    assert(mprotect(qr.base, qr.accessible, PROT_READ | PROT_WRITE) == 0);
    assert(mprotect(ar.base, ar.accessible, PROT_READ | PROT_WRITE) == 0);
}
static void stream_ack_prefix_readonly_fixtures(void) {
    stream_fixture f; stream_fixture_init(&f);
    region qr = allocate_region(sizeof(picoquic_stream_head_t), false);
    region ar = allocate_region(DMSG_STREAM_SCAN_LIMIT * sizeof(picoquic_sack_item_t), true);
    picoquic_stream_head_t *qs = qr.object; picoquic_sack_item_t *ranges = ar.object;
    *qs = f.quic_streams[0];
    f.cnx.stream_tree.root = &qs->stream_node;
    qs->sack_list.ack_tree = (picosplay_tree_t){.comp = forbidden_sack_compare,
        .create = forbidden_sack_create, .delete_node = forbidden_sack_delete,
        .node_value = forbidden_sack_value};
    dmsg_control *c = dmsg_control_new(); assert(c); select_lane(c, &f, 0, 100);

    stream_ack_case(&f, c, qr, ar, 0); /* Empty tree, positive sent extent. */
    qs->sent_offset = 0; stream_ack_case(&f, c, qr, ar, 0);
    qs->sent_offset = 10;
    ranges[0] = (picoquic_sack_item_t){.start_of_sack_range = 0, .end_of_sack_range = 3};
    qs->sack_list.ack_tree.root = &ranges[0].node;
    stream_ack_case(&f, c, qr, ar, 4); /* Inclusive normal prefix. */
    ranges[0].start_of_sack_range = 1; stream_ack_case(&f, c, qr, ar, 0);
    ranges[0].start_of_sack_range = 0; ranges[0].end_of_sack_range = 9;
    stream_ack_case(&f, c, qr, ar, 10); /* Full bytes, no FIN. */
    ranges[0].end_of_sack_range = 8; stream_ack_case(&f, c, qr, ar, 9);

    /* Merged production shape, and hand-made adjacency/overlap controls. The
     * latter are not emitted by update_sack_list, but exercise prefix extension. */
    ranges[0].end_of_sack_range = 7; stream_ack_case(&f, c, qr, ar, 8);
    ranges[0].end_of_sack_range = 3;
    ranges[1] = (picoquic_sack_item_t){.node.left = &ranges[0].node,
        .start_of_sack_range = 4, .end_of_sack_range = 7};
    ranges[0].node.parent = &ranges[1].node;
    qs->sack_list.ack_tree.root = &ranges[1].node;
    stream_ack_case(&f, c, qr, ar, 8);
    ranges[1].start_of_sack_range = 2; stream_ack_case(&f, c, qr, ar, 8);
    ranges[1].start_of_sack_range = 5; stream_ack_case(&f, c, qr, ar, 4); /* Real byte-4 hole. */
    qs->fin_sent = 1; ranges[1].end_of_sack_range = 10;
    stream_ack_case(&f, c, qr, ar, 4); /* ACKed FIN/suffix cannot bridge the hole. */

    qs->sack_list.ack_tree.root = &ranges[0].node;
    ranges[0].node = (picosplay_node_t){.parent = ar.guard}; /* Never follow parent. */
    ranges[0].end_of_sack_range = 10;
    stream_ack_case(&f, c, qr, ar, 10); /* Merged data+FIN is clamped to bytes. */
    ranges[0].start_of_sack_range = 10; stream_ack_case(&f, c, qr, ar, 0); /* FIN only. */
    qs->sent_offset = 0; ranges[0].start_of_sack_range = ranges[0].end_of_sack_range = 0;
    stream_ack_case(&f, c, qr, ar, 0); /* Zero-byte FIN is known zero, not one byte. */
    qs->fin_sent = 0; stream_ack_case(&f, c, qr, ar, UINT64_MAX); /* FIN marker without sent FIN. */
    qs->sent_offset = 10; ranges[0].end_of_sack_range = 10;
    stream_ack_case(&f, c, qr, ar, UINT64_MAX);
    qs->fin_sent = 1; ranges[0].end_of_sack_range = 11;
    stream_ack_case(&f, c, qr, ar, UINT64_MAX); /* Beyond sent extent even with FIN. */
    ranges[0].end_of_sack_range = UINT64_MAX;
    stream_ack_case(&f, c, qr, ar, UINT64_MAX); /* Historical zero-offset underflow. */
    ranges[0].start_of_sack_range = 5; ranges[0].end_of_sack_range = 4;
    stream_ack_case(&f, c, qr, ar, UINT64_MAX); /* Historical positive-offset empty frame. */
    qs->sent_offset = (UINT64_C(1) << 62) - 1;
    ranges[0].start_of_sack_range = 0; ranges[0].end_of_sack_range = qs->sent_offset;
    stream_ack_case(&f, c, qr, ar, qs->sent_offset); /* Maximum valid data+FIN end. */
    qs->fin_sent = 0; --ranges[0].end_of_sack_range;
    stream_ack_case(&f, c, qr, ar, qs->sent_offset); /* Maximum valid data end. */
    ++qs->sent_offset; qs->sack_list.ack_tree.root = ar.guard;
    stream_ack_case(&f, c, qr, ar, UINT64_MAX); /* Invalid extent rejected before tree access. */

    qs->sent_offset = 65;
    for (unsigned i = 0; i < DMSG_STREAM_SCAN_LIMIT; ++i)
        ranges[i] = (picoquic_sack_item_t){.node.parent = ar.guard,
            .node.left = i ? &ranges[i - 1].node : NULL,
            .start_of_sack_range = i, .end_of_sack_range = i};
    qs->sack_list.ack_tree.root = &ranges[62].node;
    stream_ack_case(&f, c, qr, ar, 63); /* 63 entries, finite tree. */
    qs->sack_list.ack_tree.root = &ranges[63].node;
    stream_ack_case(&f, c, qr, ar, 64); /* 64 descents/stack slots, finite tree. */
    ranges[0].node.left = ar.guard;
    stream_ack_case(&f, c, qr, ar, UINT64_MAX); /* Unreadable 65th left node. */
    for (unsigned i = 0; i < DMSG_STREAM_SCAN_LIMIT; ++i)
        ranges[i].node = (picosplay_node_t){.parent = ar.guard,
            .right = i < DMSG_STREAM_SCAN_LIMIT - 1 ? &ranges[i + 1].node : ar.guard};
    qs->sack_list.ack_tree.root = &ranges[0].node;
    stream_ack_case(&f, c, qr, ar, UINT64_MAX); /* Unreadable 65th right node. */
    qs->sent_offset = 64; stream_ack_case(&f, c, qr, ar, 64); /* Fully proved prefix needs no node 65. */
    ranges[1].start_of_sack_range = ranges[1].end_of_sack_range = 2;
    stream_ack_case(&f, c, qr, ar, 1); /* First hole resolves before unknown suffix. */

    qs->sent_offset = 10;
    ranges[0] = (picoquic_sack_item_t){.node.left = &ranges[0].node,
        .start_of_sack_range = 0, .end_of_sack_range = 3};
    stream_ack_case(&f, c, qr, ar, UINT64_MAX); /* Left self-cycle fills only bounded stack. */
    ranges[0].node.left = NULL; ranges[0].node.right = &ranges[0].node;
    stream_ack_case(&f, c, qr, ar, UINT64_MAX); /* Right self-cycle cannot resolve prefix. */
    ranges[0].node.right = &ranges[1].node;
    ranges[1] = (picoquic_sack_item_t){.node.left = &ranges[0].node,
        .start_of_sack_range = 4, .end_of_sack_range = 7};
    stream_ack_case(&f, c, qr, ar, UINT64_MAX); /* Two-node left/right cycle. */
    printf("C sender ACK prefix: %u protected manual SACK cases/FIN byte clamp/invalid/64 entries/guarded node65/cycles/no mutation PASS\n", sack_cases);
    release(qr); release(ar); stream_fixture_close(&f); dmsg_control_free(c);
}
static void *stream_writer(void *arg) {
    concurrent_fixture *f = arg; stream_fixture source; stream_fixture_init(&source);
    picoquic_sack_item_t ranges[2] = {0};
    for (unsigned lane = 0; lane < 2; ++lane)
        source.quic_streams[lane].sack_list.ack_tree.root = &ranges[lane].node;
    pthread_barrier_wait(&f->barrier);
    for (uint64_t g = 1; g <= WRITES; ++g) {
        unsigned lane = g % 2;
        ranges[lane].end_of_sack_range = g % source.quic_streams[lane].sent_offset;
        select_lane(f->control, &source, lane, g);
        dmsg_stream_publish(f->control, &source.conn, 1000 + g);
        if (g % 32 == 0) sched_yield();
        if (g == WRITES / 2)
            while (!atomic_load_explicit(&f->intermediate, memory_order_acquire)) sched_yield();
    }
    stream_fixture_close(&source);
    atomic_store_explicit(&f->done, true, memory_order_release); return NULL;
}
static void *stream_reader(void *arg) {
    concurrent_fixture *f = arg; pthread_barrier_wait(&f->barrier);
    do {
        dmsg_stream_snapshot out; loads = 0;
        if (dmsg_control_stream_snapshot(f->control, &out, sizeof(out))) {
            assert(out.generation >= 1 && out.generation <= WRITES);
            dmsg_stream_snapshot expected = stream_expected(out.generation % 2, out.generation,
                1000 + out.generation);
            expected.tx_ack_prefix_end = out.generation % expected.tx_submitted + 1;
            assert(memcmp(&out, &expected, sizeof(out)) == 0);
            atomic_fetch_add_explicit(&f->accepted, 1, memory_order_relaxed);
            if (out.generation > 1 && out.generation < WRITES)
                atomic_fetch_add_explicit(&f->intermediate, 1, memory_order_release);
        } else {
            const dmsg_stream_snapshot zero = {0}; assert(memcmp(&out, &zero, sizeof(out)) == 0);
            atomic_fetch_add_explicit(&f->unavailable, 1, memory_order_relaxed);
        }
        assert(loads <= 3 * (DMSG_STREAM_WORDS + 5));
    } while (!atomic_load_explicit(&f->done, memory_order_acquire));
    return NULL;
}
static void stream_concurrent_publication(void) {
    concurrent_fixture f = {.control = dmsg_control_new()}; assert(f.control);
    atomic_init(&f.done, false); atomic_init(&f.accepted, 0); atomic_init(&f.unavailable, 0);
    atomic_init(&f.intermediate, 0);
    assert(pthread_barrier_init(&f.barrier, NULL, READERS + 1) == 0);
    pthread_t owner, readers[READERS]; assert(pthread_create(&owner, NULL, stream_writer, &f) == 0);
    for (unsigned i = 0; i < READERS; ++i) assert(pthread_create(&readers[i], NULL, stream_reader, &f) == 0);
    assert(pthread_join(owner, NULL) == 0);
    for (unsigned i = 0; i < READERS; ++i) assert(pthread_join(readers[i], NULL) == 0);
    assert(atomic_load(&f.accepted) > 0 && atomic_load(&f.unavailable) > 0 && atomic_load(&f.intermediate) > 0);
    printf("C selected stream: %u publications, %u readers, %llu coherent reads (%llu non-final), %llu unavailable\n",
        WRITES, READERS, (unsigned long long)atomic_load(&f.accepted),
        (unsigned long long)atomic_load(&f.intermediate), (unsigned long long)atomic_load(&f.unavailable));
    assert(pthread_barrier_destroy(&f.barrier) == 0); dmsg_control_free(f.control);
}

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "--stream-dto") == 0) {
        stream_fixture f; stream_fixture_init(&f);
        dmsg_control *c = dmsg_control_new(); assert(c); select_lane(c, &f, 1, 42);
        dmsg_stream_publish(c, &f.conn, 1042);
        const dmsg_stream_snapshot out = stream_read(c), expected = stream_expected(1, 42, 1042);
        assert(memcmp(&out, &expected, sizeof(out)) == 0);
        assert(fwrite(&out, sizeof(out), 1, stdout) == 1);
        stream_fixture_close(&f); dmsg_control_free(c); return 0;
    }
    if (argc == 2 && strcmp(argv[1], "--dto") == 0) {
        generation_fixture *f = calloc(1, sizeof(*f)); assert(f);
        generation(f, 42);
        dmsg_control *c = dmsg_control_new(); assert(c);
        dmsg_transport_publish(c, &f->cnx, 1042);
        const dmsg_transport_snapshot out = read_snapshot(c);
        const dmsg_transport_snapshot expected = expected_generation(42);
        assert(memcmp(&out, &expected, sizeof(out)) == 0);
        assert(fwrite(&out, sizeof(out), 1, stdout) == 1);
        free(f); dmsg_control_free(c); return 0;
    }
    availability_and_read_bound();
    ages_contexts_traps_and_scan_bounds();
    readonly_upstream_and_path_bound();
    concurrent_publication();
    stream_selection_generation_retirement_and_read_bound();
    stream_readonly_and_scan_bounds();
    stream_ack_prefix_readonly_fixtures();
    stream_concurrent_publication();
    puts("C layout/publication/unknown/zero/8 paths/64 nodes/3 attempts/traps/legacy-vs-MP/read-only PASS");
    puts("C selected stream/15 words/highest+contiguous+ACK prefix/private port/2 lanes/generation/retirement/underflow/64 list+tree/3 attempts/read-only PASS");
    return 0;
}
