package org.dmsg.client

import org.junit.Assert.*
import org.junit.Test
import uniffi.dmsg_core.DeleteScope
import uniffi.dmsg_core.DeliveryState
import uniffi.dmsg_core.HistoryMessage
import uniffi.dmsg_core.MessageDirection
import uniffi.dmsg_core.MessageKind
import uniffi.dmsg_core.VoiceInfo

class VoiceNotesTest {
    private val contact = Dialog("P", "accepted", hasKeys = true)
    private fun row(id: Long = 1, kind: MessageKind = MessageKind.VOICE) = HistoryMessage(
        localId = id, messageIdHex = id.toString(16).padStart(32, '0'), contactId = "P",
        direction = MessageDirection.OUTGOING, kind = kind,
        voice = if (kind == MessageKind.VOICE) VoiceInfo(32_000u, byteArrayOf(20, 80), 200u, true) else null,
        text = if (kind == MessageKind.TEXT) "Text remains selectable" else "", localTimestampMs = id,
        deliveryState = DeliveryState.ACCEPTED, serverSeq = id, serverTimestampMs = id,
        revision = 0uL, hiddenSelf = false, deletedAll = false, changeDeliveryState = null)
    private fun facade() = FakeFacade().apply { dialogs.add(contact) }

    @Test fun holdReleaseLocksAndCancellationAreTerminalForTheGesture() {
        val state = VoiceUiState()
        assertTrue(state.begin()); assertFalse(state.begin())
        assertEquals(VoiceGesture.None, state.move(-63f, -10f, 64f))
        assertEquals(VoiceGesture.Lock, state.move(-10f, -64f, 64f))
        assertEquals(VoiceMode.Locked, state.mode)
        assertEquals(VoiceGesture.None, state.move(-200f, -200f, 64f))
        state.discard(); assertTrue(state.begin())
        assertEquals(VoiceGesture.Cancel, state.move(-64f, -200f, 64f))
        state.discard(); state.finish(true)
        assertEquals(VoiceMode.Idle, state.mode); assertFalse(state.sendOnFinish)
        state.begin(); state.finish(true)
        assertEquals(VoiceMode.Finishing, state.mode); assertTrue(state.sendOnFinish)
    }

    @Test fun backgroundCancelsSendIntentAndRetainsUnsentPreviewOnlyInMemory() {
        for (mode in listOf(VoiceMode.Holding, VoiceMode.Locked, VoiceMode.Pausing, VoiceMode.Paused, VoiceMode.Finishing)) {
            val state = VoiceUiState().apply { this.mode = mode; sendOnFinish = true }
            state.background()
            assertEquals(VoiceMode.Finishing, state.mode); assertFalse(state.sendOnFinish)
            val bytes = byteArrayOf(1, 2)
            state.preview(bytes, 16_001, byteArrayOf(5))
            assertEquals(VoiceMode.Preview, state.mode); assertEquals("0:01", voiceTime(state.samples))
            state.discard(); assertNull(state.bytes); assertTrue(bytes.all { it == 0.toByte() })
            assertFalse(VoiceUiState().busy)
        }
    }

    @Test fun cachedSessionQueuesOfflineWithoutPrimingOrWaitingForTransport() {
        val f = facade().apply { retryFailure = DmsgError(R.string.error_transport, ErrorKind.Transport) }
        val attempt = VoiceSendAttempt("P")
        assertTrue(attempt.mid.matches(Regex("[0-9a-f]{32}")))
        assertNotEquals(attempt.mid, VoiceSendAttempt("P").mid)
        val result = TextSendCoordinator.queueVoice(f, attempt, byteArrayOf(1)) as VoiceSendOutcome.Saved
        assertEquals(attempt.mid, result.row.messageIdHex)
        assertEquals(0, f.primeCalls); assertEquals(0, f.retryCalls)
        assertEquals(DeliveryState.QUEUED, result.row.deliveryState)
        assertTrue(TextSendCoordinator.queueVoice(f, attempt, byteArrayOf(1)) is VoiceSendOutcome.Saved)
        assertEquals(1, f.history.size)
    }

    @Test fun onlyMissingSessionBootstrapsAndBootstrapFailureKeepsDraftUnwritten() {
        val f = facade().apply { sessionReady = false; sendFailure = DmsgError(R.string.error_transport, ErrorKind.Transport) }
        assertTrue(TextSendCoordinator.queueVoice(f, VoiceSendAttempt("P"), byteArrayOf(1)) is VoiceSendOutcome.NotSaved)
        assertEquals(1, f.primeCalls); assertEquals(0, f.voiceQueueCalls); assertTrue(f.history.isEmpty())
        f.sendFailure = null
        assertTrue(TextSendCoordinator.queueVoice(f, VoiceSendAttempt("P"), byteArrayOf(1)) is VoiceSendOutcome.Saved)
        assertEquals(2, f.primeCalls); assertEquals(1, f.voiceQueueCalls)
    }

    @Test fun postCommitExceptionUsesExactMidIncludingHiddenRowsAndNeverResends() {
        val f = facade().apply { failAfterInsert = true }
        val attempt = VoiceSendAttempt("P")
        val saved = TextSendCoordinator.queueVoice(f, attempt, byteArrayOf(1)) as VoiceSendOutcome.Saved
        assertTrue(saved.recovered); assertEquals(1, f.voiceQueueCalls)
        f.history[0] = f.history[0].copy(hiddenSelf = true, voice = null)
        assertTrue(reconcileVoiceSend(f, attempt, DmsgError(R.string.error_store)) is VoiceSendOutcome.Saved)
        assertEquals(1, f.voiceQueueCalls)
        assertTrue(reconcileVoiceSend(f, VoiceSendAttempt("P"), DmsgError(R.string.error_store)) is VoiceSendOutcome.NotSaved)
    }

    @Test fun unresolvedVoiceBlocksCrossChatTextVoiceAndDeleteUntilExactReadCompletes() {
        val f = facade().apply { failAfterInsert = true; exactVoiceReadFailure = true }
        val attempt = TextSendCoordinator.queueVoice(f, VoiceSendAttempt("P"), byteArrayOf(1)) as VoiceSendOutcome.Uncertain
        try {
            assertEquals(attempt, TextSendCoordinator.pendingVoiceFor("P"))
            assertTrue(TextSendCoordinator.send(f, "OTHER", "text") is TextSendOutcome.NotSaved)
            assertTrue(TextSendCoordinator.queueVoice(f, VoiceSendAttempt("OTHER"), byteArrayOf(1)) is VoiceSendOutcome.NotSaved)
            assertTrue(TextSendCoordinator.mutate(f, "P", 1, MessageActionCommand.Delete(DeleteScope.SELF_ONLY)) is MessageActionOutcome.NotSaved)
            assertEquals(1, f.voiceQueueCalls); assertEquals(0, f.mutationCalls)
        } finally { f.exactVoiceReadFailure = false; TextSendCoordinator.reconcileVoice(f, attempt) }
        val proof = TextSendCoordinator.reconcileVoice(f, attempt)
        assertTrue(proof is VoiceSendOutcome.Saved); assertNull(TextSendCoordinator.pendingVoiceFor("P"))
        f.history.clear() // A proved commit cannot later be reclassified as not saved.
        assertEquals(proof, TextSendCoordinator.reconcileVoice(f, attempt))
    }

    @Test fun voiceHasTextDeleteScopesButCannotEditAndQueuedSelfHideDoesNotCancelUpload() {
        val original = row()
        assertFalse(canEditMessage(original, contact)); assertFalse(MessageComposer().start(original))
        assertEquals(listOf(DeleteScope.SELF_ONLY, DeleteScope.EVERYONE), deleteScopes(original, contact))
        val queued = original.copy(deliveryState = DeliveryState.QUEUED, serverSeq = null, serverTimestampMs = null)
        val f = facade().apply { history.add(queued) }
        val hidden = TextSendCoordinator.mutate(f, "P", 1, MessageActionCommand.Delete(DeleteScope.SELF_ONLY)) as MessageActionOutcome.Saved
        assertTrue(hidden.row.hiddenSelf); assertNull(hidden.row.voice); assertEquals(0, f.retryCalls)
        assertEquals(original.messageIdHex, f.pendingVoiceUpload()!!.messageIdHex)
        assertEquals(DeliveryState.QUEUED, f.messageStatus(original.messageIdHex))
    }

    @Test fun staleRowsCannotRestoreDeletedVoicePayloadOrReviveItsStableViewKey() {
        val original = row()
        val key = VoiceKey(original)
        val window = HistoryWindow().apply { latest(uniffi.dmsg_core.HistoryPage(listOf(original), null)) }
        window.replace(original.copy(hiddenSelf = true, voice = null))
        window.replace(original)
        assertTrue(window.rows.single().hiddenSelf); assertNull(window.rows.single().voice)
        window.latest(uniffi.dmsg_core.HistoryPage(listOf(original), null))
        window.older(uniffi.dmsg_core.HistoryPage(listOf(original), null))
        assertTrue(window.rows.single().hiddenSelf); assertNull(window.rows.single().voice)
        assertFalse(key.matches(window.rows.single()))
        assertFalse(key.matches(original.copy(messageIdHex = "f".repeat(32))))
    }

    @Test fun equalNativeWaveformArraysDoNotInvalidateTextSelectionOnNoOpRefresh() {
        val text = row(2, MessageKind.TEXT)
        val voice = row()
        val refreshed = voice.copy(voice = voice.voice!!.copy(waveform = voice.voice!!.waveform.copyOf()))
        assertTrue(sameHistoryRows(listOf(voice, text), listOf(refreshed, text.copy())))
        assertFalse(sameHistoryRows(listOf(voice, text), listOf(refreshed.copy(voice = refreshed.voice!!.copy(downloaded = false)), text)))
        assertFalse(sameHistoryRows(listOf(voice, text), listOf(refreshed, text.copy(text = "changed"))))
    }

    @Test fun mixedTimelinePagesKeepStableIdsLocalReadSemanticsAndVoiceSummaryMetadata() {
        val f = facade()
        (1L..601L).forEach { id -> f.history.add(row(id, if (id % 2L == 0L) MessageKind.TEXT else MessageKind.VOICE)
            .copy(direction = MessageDirection.INCOMING, deliveryState = null)) }
        val window = HistoryWindow().apply { latest(f.timelinePage("P", null, 50)) }
        val anchor = window.rows.first().localId
        while (window.nextBefore != null) window.older(f.timelinePage("P", window.nextBefore, 50))
        assertEquals((1L..601L).toList(), window.rows.map { it.localId })
        assertTrue(window.rows.any { it.localId == anchor }); assertEquals(601, window.rows.map { it.messageIdHex }.distinct().size)
        assertEquals(601uL, f.summary("P")!!.localUnread)
        assertEquals(MessageKind.VOICE, f.summary("P")!!.previewKind)
        assertEquals(2000u, f.summary("P")!!.voiceDurationMs)
        assertEquals(3L, f.markRead("P", window.viewedAnchor(listOf(2, 3))!!))
        assertEquals(598uL, f.summary("P")!!.localUnread)
    }
}
