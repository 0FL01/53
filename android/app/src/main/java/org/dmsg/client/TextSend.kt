package org.dmsg.client

import uniffi.dmsg_core.MessageDirection
import java.util.WeakHashMap
import java.util.concurrent.atomic.AtomicLong

private val attempts = AtomicLong()

internal sealed interface TextSendOutcome {
    data class Saved(val messageId: String, val recoveredAfterError: Boolean = false) : TextSendOutcome
    data class NotSaved(val error: Throwable) : TextSendOutcome
    data class Uncertain(val baseline: Long, val text: String, val attemptId: Long = attempts.incrementAndGet()) : TextSendOutcome
}

/** Across chat instances, a second plaintext write must not obscure an unresolved first write. */
internal object TextSendCoordinator {
    @Volatile private var unresolved: Pair<String, TextSendOutcome.Uncertain>? = null
    // Weak keys retain proof exactly while an activity/VM may still deliver that attempt.
    private val completed = WeakHashMap<TextSendOutcome.Uncertain, TextSendOutcome>()
    fun pendingFor(contactId: String) = unresolved?.takeIf { it.first == contactId }?.second
    fun send(f: DmsgFacade, contactId: String, text: String): TextSendOutcome {
        unresolved?.let { (peer, attempt) ->
            if (reconcile(f, peer, attempt) is TextSendOutcome.Uncertain)
                return TextSendOutcome.NotSaved(DmsgError("Сначала дождитесь проверки незавершённой отправки в предыдущем чате"))
        }
        return sendTextSafely(f, contactId, text).also { if (it is TextSendOutcome.Uncertain) unresolved = contactId to it }
    }
    fun reconcile(f: DmsgFacade, contactId: String, attempt: TextSendOutcome.Uncertain): TextSendOutcome {
        completed[attempt]?.let { return it }
        val result = reconcileTextSend(f, contactId, attempt.baseline, attempt.text)
        if (result is TextSendOutcome.Uncertain) return attempt
        completed[attempt] = result
        if (unresolved?.first == contactId && unresolved?.second == attempt) unresolved = null
        return result
    }
}

/**
 * Execute on Core.dispatch's single command worker, including the reconciliation reads.
 * It is the Android plaintext writer; the FGS only retries existing ciphertext. Core send
 * can fail AFTER its transaction commits, so a failed network call is not proof of no row.
 */
internal fun sendTextSafely(f: DmsgFacade, contactId: String, text: String): TextSendOutcome {
    val baseline = try { f.historyPage(contactId, null, 1).rows.firstOrNull()?.localId ?: 0L }
        catch (e: Exception) { return TextSendOutcome.NotSaved(e) } // did not call send
    return try { TextSendOutcome.Saved(f.send(contactId, text)) }
        catch (e: Exception) { reconcileTextSend(f, contactId, baseline, text, e) }
}

internal fun reconcileTextSend(f: DmsgFacade, contactId: String, baseline: Long, text: String,
    error: Throwable = DmsgError("Отправка не была сохранена. Черновик сохранён")): TextSendOutcome {
    return try {
        var before: Long? = null
        do {
            val page = f.historyPage(contactId, before, 100)
            val fresh = page.rows.filter { it.localId > baseline }
            val saved = fresh.firstOrNull { it.direction == MessageDirection.OUTGOING && it.text == text }
            if (saved != null) return TextSendOutcome.Saved(saved.messageIdHex, recoveredAfterError = true)
            if (page.rows.any { it.localId <= baseline }) break
            before = page.nextBeforeLocalId
        } while (before != null)
        TextSendOutcome.NotSaved(error)
    } catch (_: Exception) { TextSendOutcome.Uncertain(baseline, text) }
}
