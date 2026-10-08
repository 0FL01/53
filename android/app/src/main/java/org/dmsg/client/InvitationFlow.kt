package org.dmsg.client

import java.security.SecureRandom

enum class InvitationState { ACTIVE, USED, REVOKED, EXPIRED }
data class InvitationInfo(val issueId: ByteArray, val createdAt: Long, val expiresAt: Long)
data class InvitationList(val serverNow: Long, val invitations: List<InvitationInfo>)

/** Mutable foreground owner; deliberately no data-class secret toString/copy. */
class InvitationGrant(val serverNow: Long, val invitation: InvitationInfo, val state: InvitationState,
    private var phrase: CharArray?) {
    fun phrase(): String? = phrase?.concatToString()
    fun clear() { phrase?.fill('\u0000'); phrase = null }
    override fun toString() = "InvitationGrant([redacted])"
}

/** UI-thread state machine. Jobs run on Core's existing command queue; results are fenced. */
internal class InvitationFlow(private val randomId: () -> ByteArray = {
    ByteArray(16).also { SecureRandom().nextBytes(it) }
}) {
    class Job(val stamp: Long, val issueAttempt: Boolean, val run: (DmsgFacade) -> Result)
    class Result(val list: InvitationList?, val grant: InvitationGrant?, val revoked: Boolean = false) {
        fun clear() { grant?.clear() }
    }
    private var generation = 0L
    var busy = false; private set
    var pending: ByteArray? = null; private set
    var selected: ByteArray? = null; private set
    var pendingRevoke: ByteArray? = null; private set
    var grant: InvitationGrant? = null; private set
    var rows: List<InvitationInfo> = emptyList(); private set
    var serverNow = 0L; private set
    val mayShare get() = !busy && pendingRevoke == null && grant?.state == InvitationState.ACTIVE && grant?.phrase() != null

    fun restore(pending: ByteArray?, selected: ByteArray?, revoke: ByteArray?) {
        this.pending = valid(pending); this.selected = valid(selected); pendingRevoke = valid(revoke)
    }
    private fun valid(id: ByteArray?) = id?.takeIf { it.size == 16 }?.copyOf()
    private fun begin(issueAttempt: Boolean = false, run: (DmsgFacade) -> Result): Job? {
        if (busy) return null
        busy = true
        grant?.clear(); grant = null
        return Job(++generation, issueAttempt, run)
    }
    fun refresh(): Job? {
        val id = (pending ?: selected)?.copyOf()
        val revoke = pendingRevoke?.copyOf()
        return begin { f ->
            if (revoke != null) f.revokeInvitation(revoke)
            val list = f.listInvitations()
            if (list.invitations.size > 8) throw DmsgError(R.string.error_protocol, ErrorKind.Protocol)
            Result(list, id?.let(f::issueInvitation), revoke != null)
        }
    }
    fun create(): Job? {
        if (busy || pendingRevoke != null) return null
        if (pending == null) {
            if (rows.size >= 8) throw DmsgError(R.string.error_invite_limit, ErrorKind.InviteLimit)
            pending = randomId().also { require(it.size == 16) }.copyOf()
        }
        val id = pending!!.copyOf()
        selected = id.copyOf()
        return begin(true) { Result(null, it.issueInvitation(id)) }
    }
    fun select(id: ByteArray): Job? {
        if (busy || pending != null || pendingRevoke != null || rows.none { it.issueId.contentEquals(id) }) return null
        selected = id.copyOf()
        return begin { Result(null, it.issueInvitation(id.copyOf())) }
    }
    fun revoke(): Job? {
        if (busy) return null
        val id = selected?.copyOf() ?: return null
        pendingRevoke = id.copyOf()
        return begin { f ->
            f.revokeInvitation(id)
            Result(f.listInvitations(), f.issueInvitation(id), true)
        }
    }
    fun complete(job: Job, result: kotlin.Result<Result>): Boolean {
        if (job.stamp != generation) { result.getOrNull()?.clear(); return false }
        busy = false
        // A typed server rejection is a known outcome, unlike a lost/invalid response.
        // Release the never-issued id so the user can select and revoke an existing row.
        val error = result.exceptionOrNull() as? DmsgError
        if (job.issueAttempt && pending != null && pendingRevoke == null && error?.kind in setOf(
                ErrorKind.InviteLimit, ErrorKind.AuthRateLimited, ErrorKind.InvalidInput,
                ErrorKind.NotAuthenticated, ErrorKind.Revoked)) {
            if (selected.contentEquals(pending)) selected = null
            pending = null
        }
        result.getOrNull()?.let { value ->
            value.list?.let { rows = it.invitations.take(8); serverNow = it.serverNow }
            if (value.revoked) pendingRevoke = null
            value.grant?.let {
                if (!it.invitation.issueId.contentEquals(selected)) {
                    it.clear(); throw DmsgError(R.string.error_protocol, ErrorKind.Protocol)
                }
                pending = null
                grant = it
                serverNow = it.serverNow
                rows = rows.filterNot { row -> row.issueId.contentEquals(it.invitation.issueId) }
                if (it.state == InvitationState.ACTIVE) rows = (rows + it.invitation).take(8)
                else it.clear()
            }
        }
        return true
    }
    fun pause() { generation++; busy = false; grant?.clear(); grant = null }
    fun expire() { grant?.clear(); grant = null }
}
