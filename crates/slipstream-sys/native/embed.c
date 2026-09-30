#include "embed.h"
#include "slipstream.h"
#include <arpa/inet.h>
#include <stdlib.h>
#include <openssl/crypto.h>

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
    atomic_store_explicit(&c->snapshot,
        phase | ((uint64_t)port << 8) | ((uint64_t)(uint32_t)error << 24),
        memory_order_release);
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
