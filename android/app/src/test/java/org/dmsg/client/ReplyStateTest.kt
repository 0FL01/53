package org.dmsg.client

import org.junit.Assert.*
import org.junit.Test
import uniffi.dmsg_core.DeliveryState
import uniffi.dmsg_core.HistoryMessage
import uniffi.dmsg_core.HistoryPage
import uniffi.dmsg_core.MessageDirection
import uniffi.dmsg_core.MessageKind
import uniffi.dmsg_core.ReplyInfo
import uniffi.dmsg_core.ReplyTargetState

class ReplyStateTest {
    private fun row(id: Long = 1, kind: MessageKind = MessageKind.TEXT) = HistoryMessage(
        localId = id, messageIdHex = id.toString(16).padStart(32, '0'), contactId = "P", direction = MessageDirection.OUTGOING,
        text = "original $id", localTimestampMs = id, deliveryState = DeliveryState.ACCEPTED, serverSeq = id,
        serverTimestampMs = id, revision = 0uL, hiddenSelf = false, deletedAll = false, changeDeliveryState = null,
        kind = kind, voice = null, reply = null)
    private fun info(id: Long? = 1, revision: ULong? = 0uL, state: ReplyTargetState = ReplyTargetState.AVAILABLE) =
        ReplyInfo(id, state, revision, MessageDirection.INCOMING, MessageKind.TEXT, if (state == ReplyTargetState.AVAILABLE) "current" else "", null)
    private fun unavailable(outcome: VoiceSendOutcome) {
        assertTrue(outcome is VoiceSendOutcome.NotSaved)
        assertEquals(ErrorKind.MessageUnavailable, ((outcome as VoiceSendOutcome.NotSaved).error as DmsgError).kind)
    }

    @Test fun replyEligibilityIsIndependentOfOwnRemoteActions() {
        for (kind in listOf(MessageKind.TEXT, MessageKind.VOICE)) for (direction in MessageDirection.values()) {
            val target = row(kind = kind).copy(direction = direction, deliveryState = DeliveryState.QUEUED)
            assertTrue(canReplyMessage(target))
            assertFalse(canEditMessage(target, Dialog("P", "accepted", hasKeys = true)))
            assertFalse(canReplyMessage(target.copy(hiddenSelf = true)))
            assertFalse(canReplyMessage(target.copy(deletedAll = true)))
        }
    }

    @Test fun submittedRawTextAndSelectorAreAnAtomicDraftTuple() {
        val source = " \n**raw Ж** e\u0301 👩‍💻\n "
        val draft = OutgoingDraft().apply { text = source; reply = ReplyDraft(1, row()) }
        assertEquals(source, draft.begin()); assertEquals(1L, draft.submittedReplyLocalId)
        assertNull(draft.begin())
        draft.reply = ReplyDraft(2, row(2))
        draft.finish(true)
        assertEquals(source, draft.text); assertEquals(2L, draft.reply!!.targetLocalId)
        assertNull(draft.submittedText); assertNull(draft.submittedReplyLocalId)
        draft.begin(); draft.text = "replacement"; draft.finish(true)
        assertEquals("replacement", draft.text); assertNotNull(draft.reply)
        draft.begin(); draft.finish(false)
        assertEquals("replacement", draft.text); assertNotNull(draft.reply)
        draft.begin(); draft.finish(true)
        assertEquals("", draft.text); assertNull(draft.reply)
    }

    @Test fun recoveryRestoresActualAttemptWithoutRecapturingUnrelatedVisibleTuple() {
        for (draft in listOf(OutgoingDraft().apply { text = "different" }, OutgoingDraft().apply { reply = ReplyDraft(2) })) {
            val oldText = draft.text; val oldReply = draft.reply
            draft.restoreSubmitted("submitted", 1)
            assertEquals("submitted", draft.submittedText); assertEquals(1L, draft.submittedReplyLocalId)
            assertEquals(oldText, draft.text); assertSame(oldReply, draft.reply)
            draft.finish(true)
            assertEquals(oldText, draft.text); assertSame(oldReply, draft.reply)
        }
        val empty = OutgoingDraft()
        empty.restoreSubmitted("submitted", 1)
        assertEquals("submitted", empty.text); assertEquals(1L, empty.reply!!.targetLocalId)
        empty.finish(true); assertEquals("", empty.text); assertNull(empty.reply)
        val noReply = OutgoingDraft().apply { restoreSubmitted("plain", null) }
        noReply.finish(true); assertEquals("", noReply.text)
    }

    @Test fun editNeverConsumesNormalReplyOrRawText() {
        val composer = MessageComposer().apply { normal.text = "normal"; normal.reply = ReplyDraft(2, row(2)) }
        val normalReply = composer.normal.reply
        assertTrue(composer.start(row()))
        composer.text = "edited"; composer.cancel()
        assertEquals("normal", composer.text); assertSame(normalReply, composer.normal.reply)
        composer.start(row()); composer.saved(1)
        assertSame(normalReply, composer.normal.reply); assertEquals("normal", composer.text)
    }

    @Test fun cacheUsesCanonicalBodyAndRejectsCancelledSameIdCompletion() {
        val captured = ReplyDraft(1, row())
        val reselected = ReplyDraft(1, row().copy(text = "new intent"))
        assertFalse(applyReplyRefresh(reselected, captured, Result.success(row().copy(text = "stale"))))
        assertEquals("new intent", reselected.target!!.text)
        assertFalse(applyReplyRefresh(null, captured, Result.success(row())))
        assertTrue(applyReplyRefresh(reselected, reselected, Result.success(row().copy(text = "canonical", revision = 2uL))))
        assertEquals("canonical", reselected.target!!.text)
        reselected.refresh(row().copy(text = "old", revision = 1uL))
        assertEquals("canonical", reselected.target!!.text)
        reselected.refresh(row().copy(text = "", hiddenSelf = true, revision = 2uL))
        reselected.refresh(row().copy(text = "stale visible", revision = 2uL))
        assertFalse(reselected.available); assertEquals("", reselected.target!!.text)
        applyReplyRefresh(reselected, reselected, Result.failure(DmsgError(R.string.error_store)))
        assertTrue(reselected.failed); assertNull(reselected.target); assertFalse(reselected.available)
    }

    @Test fun targetProjectionMergesIndependentlyOfHigherSourceRevision() {
        val original = row(10).copy(revision = 3uL, reply = info())
        val window = HistoryWindow().apply { latest(HistoryPage(listOf(original), null)) }
        val targetEdited = info(revision = 2uL).copy(preview = "edited target")
        window.replace(original.copy(revision = 1uL, text = "old parent", reply = targetEdited))
        assertEquals(3uL, window.rows.single().revision); assertEquals(original.text, window.rows.single().text)
        assertEquals(targetEdited, window.rows.single().reply)
        window.replace(original.copy(revision = 4uL, reply = info(revision = 1uL)))
        assertEquals(targetEdited, window.rows.single().reply)
        window.replace(original.copy(revision = 5uL, reply = info(null, null, ReplyTargetState.MISSING)))
        assertEquals(targetEdited, window.rows.single().reply)
        val hidden = info(revision = 2uL, state = ReplyTargetState.HIDDEN)
        window.replace(original.copy(reply = hidden))
        window.replace(original.copy(revision = 6uL, reply = targetEdited))
        assertEquals(hidden, window.rows.single().reply)
        val deleted = info(revision = 3uL, state = ReplyTargetState.DELETED)
        window.replace(original.copy(reply = deleted))
        assertEquals(deleted, window.rows.single().reply)
        val unavailableOlder = info(revision = 1uL, state = ReplyTargetState.HIDDEN)
        assertEquals(unavailableOlder.copy(targetRevision = 2uL), mergeReplyInfo(targetEdited, unavailableOlder))
        assertEquals(deleted, mergeReplyInfo(deleted, unavailableOlder))
    }

    @Test fun missingCanResolveAndEqualTargetRefreshKeepsSelectionPayloadEqual() {
        val missing = info(null, null, ReplyTargetState.MISSING)
        assertEquals(info(), mergeReplyInfo(missing, info()))
        val message = row().copy(reply = info())
        val window = HistoryWindow().apply { latest(HistoryPage(listOf(message), null)) }
        window.latest(HistoryPage(listOf(message.copy(reply = message.reply!!.copy())), null))
        assertSame(message.reply, window.rows.single().reply)
        assertTrue(sameHistoryRows(listOf(message), window.rows))
    }

    @Test fun textRecoveryIncludesReplyIdentityAndTypedUnavailableIsPrecommit() {
        val f = FakeFacade().apply { history.addAll(listOf(row(), row(2))) }
        val wrong = object : DmsgFacade by f {
            override fun send(id: String, text: String, replyToLocalId: Long?): String {
                f.send(id, text, 2); throw DmsgError(R.string.error_transport, ErrorKind.Transport)
            }
        }
        assertTrue(sendTextSafely(wrong, "P", "body", 1) is TextSendOutcome.NotSaved)
        val fresh = FakeFacade().apply { history.add(row()); failAfterInsert = true }
        assertTrue(sendTextSafely(fresh, "P", "body", 1) is TextSendOutcome.Saved)
        fresh.history[0] = row().copy(hiddenSelf = true, text = "")
        assertTrue(reconcileTextSend(fresh, "P", 1, "body", replyToLocalId = 1) is TextSendOutcome.Saved)
        val typed = object : DmsgFacade by fresh {
            var reads = 0
            override fun historyPage(contactId: String, beforeLocalId: Long?, limit: Int): HistoryPage {
                if (++reads > 1) throw DmsgError(R.string.error_store)
                return fresh.historyPage(contactId, beforeLocalId, limit)
            }
            override fun send(id: String, text: String, replyToLocalId: Long?): String =
                throw DmsgError(R.string.error_message_unavailable, ErrorKind.MessageUnavailable)
        }
        assertTrue(sendTextSafely(typed, "P", "other", 1) is TextSendOutcome.NotSaved)
        assertEquals(1, typed.reads)
    }

    @Test fun uncertainTextRetainsAttemptSelectorAndCannotBeProvedByNoReplyOrSomeMissing() {
        val f = FakeFacade().apply { history.add(row()); failAfterInsert = true }
        val unreadable = object : DmsgFacade by f {
            var reads = 0
            override fun historyPage(contactId: String, beforeLocalId: Long?, limit: Int): HistoryPage {
                if (++reads > 1) throw DmsgError(R.string.error_store)
                return f.historyPage(contactId, beforeLocalId, limit)
            }
        }
        val attempt = sendTextSafely(unreadable, "P", "raw", 1) as TextSendOutcome.Uncertain
        assertEquals("raw", attempt.text); assertEquals(1L, attempt.replyToLocalId)
        assertTrue(reconcileTextSend(f, "P", attempt.baseline, attempt.text, replyToLocalId = attempt.replyToLocalId) is TextSendOutcome.Saved)
        val replied = f.history.last()
        assertFalse(replyIdentityMatches(replied, null))
        assertFalse(replyIdentityMatches(replied.copy(reply = info(null, null, ReplyTargetState.MISSING)), null))
        assertFalse(replyIdentityMatches(replied.copy(reply = null), 1))
        assertFalse(replyIdentityMatches(replied.copy(reply = info(null, null, ReplyTargetState.MISSING)), 1))
    }

    @Test fun fakeNewWritesRequireVisibleSameContactTargetsAndKeepRawTextLimitSeparate() {
        val f = FakeFacade().apply { history.addAll(listOf(row(), row(2).copy(contactId = "OTHER"), row(3).copy(hiddenSelf = true), row(4).copy(deletedAll = true))) }
        for (invalid in listOf(2L, 3L, 4L, 999L)) {
            val text = sendTextSafely(f, "P", "body", invalid) as TextSendOutcome.NotSaved
            assertEquals(ErrorKind.MessageUnavailable, (text.error as DmsgError).kind)
            unavailable(TextSendCoordinator.queueVoice(f, VoiceSendAttempt("P", replyToLocalId = invalid), byteArrayOf(1)))
        }
        assertEquals(4, f.history.size)
        val source = "😀".repeat(4000)
        assertTrue(sendTextSafely(f, "P", source, 1) is TextSendOutcome.Saved)
        assertEquals(source, f.lastSent); assertEquals(source, f.history.last().text)
        assertEquals(1L, f.history.last().reply!!.targetLocalId)
    }

    @Test fun quoteCacheIsShallowBoundedInScalarsAndClearsTerminalBodies() {
        val target = row().copy(text = "😀".repeat(170), reply = info(2).copy(preview = "nested content"))
        val preview = replyInfoForTarget(target)
        assertEquals("😀".repeat(160), preview.preview)
        assertEquals(160, preview.preview.codePointCount(0, preview.preview.length))
        assertEquals("", replyInfoForTarget(target.copy(hiddenSelf = true)).preview)
        assertEquals("", replyInfoForTarget(target.copy(deletedAll = true)).preview)
        val f = FakeFacade().apply { history.add(target.copy(reply = null)) }
        f.send("P", "reply body", 1)
        f.history[0] = target.copy(text = "canonical edit", revision = 1uL)
        assertEquals("canonical edit", f.historyMessage("P", 2).reply!!.preview)
        f.history[0] = f.history[0].copy(hiddenSelf = true, text = "")
        val hidden = f.historyMessage("P", 2).reply!!
        assertEquals(ReplyTargetState.HIDDEN, hidden.state); assertEquals("", hidden.preview)
        assertEquals(1L, hidden.targetLocalId); assertEquals(1uL, hidden.targetRevision)
    }

    @Test fun voiceFreezesReplyAcrossPausePreviewBackgroundDiscardAndPendingRecovery() {
        val state = VoiceUiState()
        state.begin(true, 1); state.mode = VoiceMode.Paused
        state.background(); state.preview(byteArrayOf(1), 16000, byteArrayOf(10))
        assertEquals(1L, state.replyToLocalId)
        state.restorePending(VoiceSendAttempt("P", replyToLocalId = 2))
        assertEquals(1L, state.replyToLocalId)
        state.discard(); assertEquals(1L, state.replyToLocalId)
        state.begin(replyToLocalId = null)
        state.restorePending(VoiceSendAttempt("P", replyToLocalId = 2))
        assertNull(state.replyToLocalId)
        val recovered = VoiceUiState().apply { restorePending(VoiceSendAttempt("P", replyToLocalId = 2)) }
        assertEquals(2L, recovered.replyToLocalId); assertEquals(VoiceMode.Queueing, recovered.mode)
        val normal = OutgoingDraft().apply { text = "keep text"; reply = ReplyDraft(1) }
        normal.restorePendingReply(2); assertEquals(1L, normal.reply!!.targetLocalId)
        val typed = OutgoingDraft().apply { text = "unrelated" }
        typed.restorePendingReply(2); assertNull(typed.reply); assertEquals("unrelated", typed.text)
        val selected = OutgoingDraft().apply { reply = ReplyDraft(1) }
        selected.restorePendingReply(2); assertEquals(1L, selected.reply!!.targetLocalId)
        val empty = OutgoingDraft().apply { restorePendingReply(2) }
        assertEquals(2L, empty.reply!!.targetLocalId)
        normal.consumeReply(2); assertEquals(1L, normal.reply!!.targetLocalId)
        normal.consumeReply(1); assertNull(normal.reply); assertEquals("keep text", normal.text)
    }

    @Test fun voiceDuplicatePrecedesSessionAndTargetAvailabilityButChecksRetainedIdentity() {
        val f = FakeFacade().apply { history.add(row()) }
        val attempt = VoiceSendAttempt("P", replyToLocalId = 1)
        val saved = TextSendCoordinator.queueVoice(f, attempt, byteArrayOf(1)) as VoiceSendOutcome.Saved
        f.history[0] = row().copy(hiddenSelf = true, text = "")
        f.history[1] = saved.row.copy(hiddenSelf = true, voice = null)
        f.sessionReady = false; f.sendFailure = DmsgError(R.string.error_identity_changed, ErrorKind.IdentityMismatch)
        assertTrue(TextSendCoordinator.queueVoice(f, attempt, byteArrayOf()) is VoiceSendOutcome.Saved)
        assertEquals(0, f.primeCalls)
        unavailable(TextSendCoordinator.queueVoice(f, attempt.copy(replyToLocalId = null), byteArrayOf()))
        unavailable(TextSendCoordinator.queueVoice(f, attempt.copy(contactId = "OTHER"), byteArrayOf()))
        assertEquals(2, f.history.size)
    }

    @Test fun voiceProofSeparatesContextMismatchFromUnreliableRowIdentity() {
        val attempt = VoiceSendAttempt("P", row().messageIdHex, 1)
        val valid = row(kind = MessageKind.VOICE).copy(reply = info(), hiddenSelf = true, voice = null)
        val f = FakeFacade()
        for (bad in listOf(valid.copy(kind = MessageKind.TEXT),
            valid.copy(reply = null), valid.copy(reply = info(2)), valid.copy(reply = info(null, null, ReplyTargetState.MISSING)))) {
            val wrong = object : DmsgFacade by f {
                override fun queueVoice(contactId: String, midHex: String, encodedBytes: ByteArray, replyToLocalId: Long?) = bad
                override fun historyMessageByMid(contactId: String, midHex: String) = bad
            }
            unavailable(TextSendCoordinator.queueVoice(wrong, attempt, byteArrayOf(1)))
            unavailable(reconcileVoiceSend(wrong, attempt, DmsgError(R.string.error_store)))
        }
        for (bad in listOf(valid.copy(contactId = "OTHER"), valid.copy(direction = MessageDirection.INCOMING),
            valid.copy(messageIdHex = "f".repeat(32)))) {
            assertTrue(proveVoiceSend(attempt, bad) is VoiceSendOutcome.Uncertain)
            val wrong = object : DmsgFacade by f {
                override fun historyMessageByMid(contactId: String, midHex: String) = bad
            }
            val error = DmsgError(R.string.error_transport, ErrorKind.Transport)
            val unresolved = reconcileVoiceSend(wrong, attempt, error) as VoiceSendOutcome.Uncertain
            assertEquals(attempt, unresolved.attempt); assertSame(error, unresolved.error)
        }
        assertTrue(proveVoiceSend(attempt, valid.copy(reply = info(state = ReplyTargetState.HIDDEN))) is VoiceSendOutcome.Saved)
        unavailable(proveVoiceSend(attempt.copy(replyToLocalId = null), valid.copy(reply = info(null, null, ReplyTargetState.MISSING))))
    }

    @Test fun voiceBootstrapsOnceOnlyAfterTypedQueueFailureAndUnavailableSkipsProofRead() {
        val f = FakeFacade()
        val typed = object : DmsgFacade by f {
            var reads = 0
            override fun queueVoice(contactId: String, midHex: String, encodedBytes: ByteArray, replyToLocalId: Long?): HistoryMessage =
                throw DmsgError(R.string.error_message_unavailable, ErrorKind.MessageUnavailable)
            override fun historyMessageByMid(contactId: String, midHex: String): HistoryMessage? { reads++; throw DmsgError(R.string.error_store) }
        }
        unavailable(TextSendCoordinator.queueVoice(typed, VoiceSendAttempt("P"), byteArrayOf(1)))
        assertEquals(0, typed.reads); assertEquals(0, f.primeCalls)
        val repeated = object : DmsgFacade by f {
            var queues = 0
            override fun voiceSessionReady(contactId: String): Boolean = error("precheck forbidden")
            override fun queueVoice(contactId: String, midHex: String, encodedBytes: ByteArray, replyToLocalId: Long?): HistoryMessage {
                queues++; throw DmsgError(R.string.voice_session_required, ErrorKind.VoiceSessionRequired)
            }
        }
        assertTrue(TextSendCoordinator.queueVoice(repeated, VoiceSendAttempt("P"), byteArrayOf(1)) is VoiceSendOutcome.NotSaved)
        assertEquals(2, repeated.queues); assertEquals(1, f.primeCalls)
    }

    @Test fun quotePagingExhaustionIncludesGapAndHiddenContinuationAndNeverMarksPrefetch() {
        val f = FakeFacade()
        (1L..10L).forEach { f.history.add(row(it).copy(serverSeq = 11 - it)) }
        val window = HistoryWindow().apply { latest(f.timelinePage("P", null, 50)) }
        assertTrue(window.pagingExhausted)
        (11L..201L).forEach { f.history.add(row(it).copy(serverSeq = 1000 - it)) }
        window.latest(f.timelinePage("P", null, 50), localHead = 201)
        assertNull(window.nextBefore); assertNotNull(window.gapBefore); assertFalse(window.pagingExhausted)
        var turns = 0
        while (window.gapBefore != null) {
            window.latest(f.timelinePage("P", window.gapBefore, 50))
            assertTrue(++turns < 10)
        }
        assertTrue(window.pagingExhausted)
        val hidden = HistoryWindow().apply { latest(HistoryPage(listOf(row().copy(hiddenSelf = true, text = "")), 1)) }
        assertFalse(hidden.pagingExhausted); assertNotNull(hidden.hiddenBefore)
        hidden.older(HistoryPage(emptyList(), null)); assertTrue(hidden.pagingExhausted)
        assertEquals(0L, f.readCursors["P"] ?: 0L)
    }
}
