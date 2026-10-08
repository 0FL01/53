/* The bundled public header, not a Rust replica of srtp_policy_t, owns ABI layout. */
#include "config.h"
#include "srtp.h"
#include <limits.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>

#if defined(OPENSSL) || defined(MBEDTLS) || defined(NSS) || defined(GCM) || \
    defined(ENABLE_DEBUG_LOGGING) || defined(ERR_REPORTING_STDOUT) || \
    defined(ERR_REPORTING_FILE)
#error "Unexpected libSRTP backend or logging configuration"
#endif

#define DMSG_MAX_PLAINTEXT_BYTES 4096
#define DMSG_AUTH_TAG_BYTES 10
#define DMSG_KEY_MATERIAL_BYTES 30

/* Volatile writes keep temporary key/plaintext erasure observable to the compiler. */
static void wipe(void *memory, size_t length)
{
    volatile unsigned char *p = (volatile unsigned char *)memory;
    while (length--)
        *p++ = 0;
}

/* All errors are static strings; packet/key contents are never formatted. */
static const char *checked(srtp_err_status_t status)
{
    switch (status) {
    case srtp_err_status_ok:
        return NULL;
    case srtp_err_status_auth_fail:
        return "SRTP authentication failed";
    case srtp_err_status_replay_fail:
        return "SRTP replay rejected";
    case srtp_err_status_replay_old:
        return "SRTP replay too old";
    case srtp_err_status_no_ctx:
        return "SRTP unexpected SSRC";
    case srtp_err_status_bad_param:
        return "Invalid SRTP packet or configuration";
    case srtp_err_status_alloc_fail:
        return "SRTP allocation failed";
    case srtp_err_status_key_expired:
        return "SRTP key expired";
    case srtp_err_status_pkt_idx_old:
    case srtp_err_status_pkt_idx_adv:
        return "SRTP packet index invalid";
    default:
        return "SRTP operation failed";
    }
}

/* Called once, behind Rust OnceLock. No shutdown/global mutation is exposed. */
const char *dmsg_srtp_init(void)
{
    srtp_err_status_t status;
    if (strcmp(srtp_get_version_string(), "libsrtp2 2.7.0") != 0)
        return "Unexpected libSRTP version";
    status = srtp_init();
    if (status != srtp_err_status_ok)
        return checked(status);
    status = srtp_install_event_handler(NULL);
    if (status != srtp_err_status_ok)
        return checked(status);
    return checked(srtp_install_log_handler(NULL, NULL));
}

const char *dmsg_srtp_create(const unsigned char *key_material,
                            uint32_t ssrc,
                            srtp_t *out)
{
    srtp_policy_t policy;
    unsigned char key[DMSG_KEY_MATERIAL_BYTES];
    srtp_err_status_t status;
    if (out == NULL || key_material == NULL)
        return "Invalid SRTP configuration";
    *out = NULL;
    memset(&policy, 0, sizeof(policy));
    srtp_crypto_policy_set_aes_cm_128_hmac_sha1_80(&policy.rtp);
    srtp_crypto_policy_set_aes_cm_128_hmac_sha1_80(&policy.rtcp);
    if (policy.rtp.cipher_key_len != DMSG_KEY_MATERIAL_BYTES ||
        policy.rtcp.cipher_key_len != DMSG_KEY_MATERIAL_BYTES ||
        policy.rtp.auth_tag_len != DMSG_AUTH_TAG_BYTES ||
        policy.rtcp.auth_tag_len != DMSG_AUTH_TAG_BYTES)
        return "Unexpected SRTP crypto policy";
    memcpy(key, key_material, sizeof(key));
    policy.key = key;
    policy.ssrc.type = ssrc_specific;
    policy.ssrc.value = ssrc;
    policy.window_size = 128;
    policy.allow_repeat_tx = 0;
    /* Zero initialization leaves no MKI, extensions, template, or next policy. */
    status = srtp_create(out, &policy);
    wipe(key, sizeof(key));
    wipe(&policy, sizeof(policy));
    if (status != srtp_err_status_ok && *out != NULL) {
        srtp_dealloc(*out);
        *out = NULL;
    }
    return checked(status);
}

void dmsg_srtp_destroy(srtp_t session)
{
    if (session != NULL)
        srtp_dealloc(session); /* Upstream erases its AES/HMAC key state. */
}

const char *dmsg_srtp_transform(srtp_t session,
                               const unsigned char *input,
                               size_t input_len,
                               unsigned char *output,
                               size_t output_capacity,
                               size_t *output_len,
                               int protect,
                               int rtcp)
{
    const size_t trailer = DMSG_AUTH_TAG_BYTES + (rtcp ? 4 : 0);
    const size_t minimum = rtcp ? 8 : 12;
    size_t plaintext_len, expected_len, allocation;
    unsigned char *buffer;
    int length;
    srtp_err_status_t status;
    if (session == NULL || input == NULL || output == NULL || output_len == NULL ||
        (protect != 0 && protect != 1) || (rtcp != 0 && rtcp != 1))
        return "Invalid SRTP operation";
    *output_len = 0;
    if (!protect && input_len < trailer)
        return "Invalid SRTP packet length";
    plaintext_len = protect ? input_len : input_len - trailer;
    if (plaintext_len < minimum || plaintext_len > DMSG_MAX_PLAINTEXT_BYTES ||
        input_len > INT_MAX)
        return "Invalid SRTP packet length";
    expected_len = plaintext_len + (protect ? trailer : 0);
    if (output_capacity < expected_len)
        return "Invalid SRTP output capacity";

    /* malloc gives the required 32-bit alignment even for unaligned Rust slices.
     * Reserve the FULL upstream maximum trailer, plus the SRTCP index, even
     * though this fixed policy only returns 10 / 14 additional bytes. */
    allocation = input_len + SRTP_MAX_TRAILER_LEN + 4;
    buffer = (unsigned char *)malloc(allocation);
    if (buffer == NULL)
        return "SRTP allocation failed";
    memset(buffer, 0, allocation);
    memcpy(buffer, input, input_len);
    length = (int)input_len;
    if (rtcp)
        status = protect ? srtp_protect_rtcp(session, buffer, &length)
                         : srtp_unprotect_rtcp(session, buffer, &length);
    else
        status = protect ? srtp_protect(session, buffer, &length)
                         : srtp_unprotect(session, buffer, &length);
    if (status == srtp_err_status_ok) {
        if (length < 0 || (size_t)length != expected_len) {
            wipe(buffer, allocation);
            free(buffer);
            return "Unexpected SRTP output length";
        }
        memcpy(output, buffer, expected_len);
        *output_len = expected_len;
    }
    /* No partially decrypted/unauthenticated packet escapes on failure. */
    wipe(buffer, allocation);
    free(buffer);
    return checked(status);
}
