/* Full-packet regression for the shared pinned SPCDNS OPT decoder. */
#include <assert.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "dns.h"

static void check(const unsigned char *options, size_t len, dns_rcode_t expected) {
    /* One root TXT question and one OPT RR; RDATA ends at the allocation boundary. */
    const unsigned char prefix[] = {
        0, 1, 1, 0, 0, 1, 0, 0, 0, 0, 0, 1,
        0, 0, 16, 0, 1,
        0, 0, 41, 4, 208, 0, 0, 0, 0, 0, 0
    };
    size_t size = sizeof(prefix) + len;
    dns_packet_t *packet = malloc(size);
    assert(packet != NULL && len <= 65535);
    memcpy(packet, prefix, sizeof(prefix));
    unsigned char *bytes = (unsigned char *)packet;
    bytes[sizeof(prefix) - 2] = (unsigned char)(len >> 8);
    bytes[sizeof(prefix) - 1] = (unsigned char)len;
    if (len) memcpy(bytes + sizeof(prefix), options, len);
    dns_decoded_t decoded[DNS_DECODEBUF_8K];
    size_t capacity = sizeof(decoded);
    dns_rcode_t result = dns_decode(decoded, &capacity, packet, size);
    if (result != expected) {
        fprintf(stderr, "OPT length=%zu expected=%d got=%d\n", len, expected, result);
        abort();
    }
    if (result == RCODE_OKAY) {
        const dns_query_t *query = (const dns_query_t *)decoded;
        assert(query->qdcount == 1 && query->arcount == 1);
    }
    free(packet);
}

int main(int argc, char **argv) {
    (void)argv;
    const unsigned char zero[] = {0xfd, 0xe9, 0, 0};
    check(zero, sizeof(zero), RCODE_OKAY); /* Previously assert(len > 4). */
    if (argc > 1) return 0;
    check(NULL, 0, RCODE_OKAY);
    const unsigned char data[] = {0xfd, 0xe9, 0, 2, 0xab, 0xcd};
    check(data, sizeof(data), RCODE_OKAY);
    const unsigned char multi[] = {0xfd, 0xe9, 0, 0, 0xfd, 0xea, 0, 2, 1, 2};
    check(multi, sizeof(multi), RCODE_OKAY);
    const unsigned char short_header[] = {0xfd, 0xe9, 0};
    for (size_t n = 1; n <= 3; n++) check(short_header, n, RCODE_FORMAT_ERROR);
    const unsigned char overrun[] = {0xfd, 0xe9, 0, 2, 0xab};
    check(overrun, sizeof(overrun), RCODE_FORMAT_ERROR);
    const unsigned char huge[] = {0xfd, 0xe9, 0xff, 0xff};
    check(huge, sizeof(huge), RCODE_FORMAT_ERROR);
    const unsigned char tail[] = {0xfd, 0xe9, 0, 0, 0xfd, 0xea, 0};
    for (size_t n = 5; n <= 7; n++) check(tail, n, RCODE_FORMAT_ERROR);
    puts("EDNS OPT: 12 full-packet cases PASS (asserts enabled)");
    return 0;
}
