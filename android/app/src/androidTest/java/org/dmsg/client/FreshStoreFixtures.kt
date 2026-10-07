package org.dmsg.client

import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.spec.IvParameterSpec
import javax.crypto.spec.SecretKeySpec

/** Test SQL writes use the current at-rest format; fixtures never require conversion. */
internal fun sealedFixtureValue(key: ByteArray, field: String, plain: ByteArray): ByteArray {
    require(key.size == 32)
    val nonce = ByteArray(12).also { SecureRandom().nextBytes(it) }
    val cipher = Cipher.getInstance("ChaCha20/Poly1305/NoPadding")
    cipher.init(Cipher.ENCRYPT_MODE, SecretKeySpec(key, "ChaCha20"), IvParameterSpec(nonce))
    cipher.updateAAD(field.toByteArray(Charsets.UTF_8))
    return "DMSG-S1".toByteArray(Charsets.US_ASCII) + nonce + cipher.doFinal(plain)
}
