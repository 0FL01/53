#ifndef DMSG_SLIPSTREAM_EMBED_H
#define DMSG_SLIPSTREAM_EMBED_H
#include <stddef.h>
#include <stdint.h>
#include <stdatomic.h>

/* Only these atomics cross threads. Every upstream runtime field stays on its
 * owning Rust worker thread. Snapshot packs phase:8, port:16, error:32. */
typedef struct {
    atomic_bool stop;
    atomic_uint_fast64_t snapshot;
} dmsg_control;
enum { DMSG_STARTING, DMSG_LISTENING, DMSG_READY, DMSG_STOPPED, DMSG_FAILED };
int dmsg_cancelled(dmsg_control *control);
void dmsg_publish(dmsg_control *control, unsigned phase, uint16_t port, int error);
int dmsg_bound(dmsg_control *control, int fd);

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
