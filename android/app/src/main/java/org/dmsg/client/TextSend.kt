package org.dmsg.client

import uniffi.dmsg_core.MessageDirection
import uniffi.dmsg_core.HistoryMessage
import uniffi.dmsg_core.DeleteScope
import java.util.WeakHashMap
import java.util.concurrent.atomic.AtomicLong

private val attempts = AtomicLong()

internal sealed interface TextSendOutcome {
    data class Saved(val messageId: String, val recoveredAfterError: Boolean = false) : TextSendOutcome
    data class NotSaved(val error: Throwable) : TextSendOutcome
    data class Uncertain(val baseline: Long, val text: String, val attemptId: Long = attempts.incrementAndGet()) : TextSendOutcome
}

internal sealed interface MessageActionCommand {
    data class Edit(val expectedRevision: ULong, val text: String) : MessageActionCommand
    data class Delete(val scope: DeleteScope) : MessageActionCommand
}
internal data class MessageActionAttempt(val before: HistoryMessage, val command: MessageActionCommand,
    val error: Throwable, val attemptId: Long = attempts.incrementAndGet())
internal sealed interface MessageActionOutcome {
    data class Saved(val row: HistoryMessage, val networkError: Throwable? = null, val recovered: Boolean = false) : MessageActionOutcome
    data class NotSaved(val error: Throwable) : MessageActionOutcome
    data class Uncertain(val attempt: MessageActionAttempt) : MessageActionOutcome
}

/** Across chat instances, a second plaintext write must not obscure an unresolved first write. */
internal object TextSendCoordinator {
    @Volatile private var unresolved: Pair<String, TextSendOutcome.Uncertain>? = null
    // Weak keys retain proof exactly while an activity/VM may still deliver that attempt.
    private val completed = WeakHashMap<TextSendOutcome.Uncertain, TextSendOutcome>()
    @Volatile private var unresolvedAction: MessageActionAttempt? = null
    private val completedActions = WeakHashMap<MessageActionAttempt, MessageActionOutcome>()
    fun pendingFor(contactId: String) = unresolved?.takeIf { it.first == contactId }?.second
    fun pendingActionFor(contactId: String) = unresolvedAction?.takeIf { it.before.contactId == contactId }
    /** Only the existing single Core.dispatch worker calls writers and reconciliation. */
    private fun blocked(f: DmsgFacade): Boolean {
        unresolved?.let { (peer, attempt) -> if (reconcile(f, peer, attempt) is TextSendOutcome.Uncertain) return true }
        unresolvedAction?.let { if (reconcileAction(f, it) is MessageActionOutcome.Uncertain) return true }
        return false
    }
    fun send(f: DmsgFacade, contactId: String, text: String): TextSendOutcome {
        if (blocked(f)) return TextSendOutcome.NotSaved(DmsgError(R.string.error_pending_send))
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
    fun mutate(f: DmsgFacade, contactId: String, localId: Long, command: MessageActionCommand): MessageActionOutcome {
        if (blocked(f)) return MessageActionOutcome.NotSaved(DmsgError(R.string.error_pending_send))
        val before = try { f.historyMessage(contactId, localId) }
            catch (e: Exception) { return MessageActionOutcome.NotSaved(e) } // writer not called
        val result = try {
            val row = when (command) {
                is MessageActionCommand.Edit -> f.editMessage(contactId, localId, command.expectedRevision, command.text)
                is MessageActionCommand.Delete -> f.deleteMessage(contactId, localId, command.scope)
            }
            MessageActionOutcome.Saved(row)
        } catch (e: Exception) {
            // Typed validation/CAS failures are precommit. Unexpected native failures need exact proof.
            if (e is DmsgError && e.kind !in setOf(ErrorKind.Other, ErrorKind.Store, ErrorKind.Transport))
                MessageActionOutcome.NotSaved(e)
            else reconcileMessageAction(f, MessageActionAttempt(before, command, e))
        }
        if (result is MessageActionOutcome.Uncertain) unresolvedAction = result.attempt
        return deliverAction(f, command, result)
    }
    fun reconcileAction(f: DmsgFacade, attempt: MessageActionAttempt): MessageActionOutcome {
        completedActions[attempt]?.let { return it }
        val result = reconcileMessageAction(f, attempt)
        if (result is MessageActionOutcome.Uncertain) return result
        val delivered = deliverAction(f, attempt.command, result)
        completedActions[attempt] = delivered
        if (unresolvedAction == attempt) unresolvedAction = null
        return delivered
    }
}

/** Delivery after durable completion cannot reclassify a saved edit/delete as not saved. */
private fun deliverAction(f: DmsgFacade, command: MessageActionCommand, outcome: MessageActionOutcome): MessageActionOutcome {
    if (outcome !is MessageActionOutcome.Saved || (command is MessageActionCommand.Delete && command.scope == DeleteScope.SELF_ONLY)) return outcome
    if (command is MessageActionCommand.Edit && outcome.row.revision == command.expectedRevision) return outcome // validated no-op
    return try { f.retry(); outcome }
        catch (e: Exception) { outcome.copy(networkError = e) }
}

internal fun reconcileMessageAction(f: DmsgFacade, attempt: MessageActionAttempt): MessageActionOutcome = try {
    val before = attempt.before
    val row = f.historyMessage(before.contactId, before.localId)
    val saved = messageVisible(before) && before.direction == MessageDirection.OUTGOING &&
        row.messageIdHex == before.messageIdHex && row.direction == MessageDirection.OUTGOING && when (val command = attempt.command) {
        is MessageActionCommand.Edit -> before.revision == command.expectedRevision &&
            row.revision == command.expectedRevision + 1uL && row.text == command.text && messageVisible(row)
        is MessageActionCommand.Delete -> when (command.scope) {
            DeleteScope.SELF_ONLY -> row.hiddenSelf && row.revision == before.revision
            DeleteScope.EVERYONE -> row.deletedAll && row.revision == before.revision + 1uL
        }
    }
    when {
        saved -> MessageActionOutcome.Saved(row, recovered = true)
        !messageVisible(row) -> MessageActionOutcome.NotSaved(DmsgError(R.string.error_message_unavailable, ErrorKind.MessageUnavailable))
        row.revision != before.revision || (attempt.command is MessageActionCommand.Edit && row.revision != attempt.command.expectedRevision) ->
            MessageActionOutcome.NotSaved(DmsgError(R.string.error_message_changed, ErrorKind.MessageChanged))
        else -> MessageActionOutcome.NotSaved(attempt.error)
    }
} catch (_: Exception) { MessageActionOutcome.Uncertain(attempt) }

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
    error: Throwable = DmsgError(R.string.error_send_not_saved)): TextSendOutcome {
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
