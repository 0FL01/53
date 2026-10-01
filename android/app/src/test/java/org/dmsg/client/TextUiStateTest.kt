package org.dmsg.client

import org.junit.Assert.*
import org.junit.Test

class TextUiStateTest {
    @Test fun serviceOwnershipIsNotReachabilityAndFailuresRetainActualSuccess() {
        val facts = ConnectionFacts()
        facts.enabled(true)
        assertNull(facts.snapshot().lastSuccessAt)
        val first = facts.begin()
        assertTrue(facts.snapshot().pollInFlight)
        facts.finish(first, 1000, null)
        facts.finish(facts.begin(), 2000, ErrorKind.Transport)
        assertEquals(1000L, facts.snapshot().lastSuccessAt)
        assertEquals(ErrorKind.Transport, facts.snapshot().lastFailure)
        val partial = facts.begin()
        facts.success(partial, 3000)
        facts.finish(partial, 4000, ErrorKind.Transport)
        assertEquals(3000L, facts.snapshot().lastSuccessAt)
        facts.enabled(false)
        assertFalse(facts.snapshot().serviceEnabled)
        assertEquals(3000L, facts.snapshot().lastSuccessAt)
    }
    @Test fun stoppedPollCannotResurrectReachability() {
        val facts = ConnectionFacts()
        facts.enabled(true)
        val poll = facts.begin()
        facts.enabled(false)
        facts.finish(poll, 1000, null)
        assertNull(facts.snapshot().lastSuccessAt)
        assertFalse(facts.snapshot().pollInFlight)
    }
    @Test fun overlappingForegroundAndWorkerChecksHaveDistinctOutcomeTokens() {
        val facts = ConnectionFacts()
        facts.enabled(true)
        val worker = facts.begin()
        val foreground = facts.begin()
        assertNotEquals(worker, foreground)
        facts.finish(worker, 1000, ErrorKind.Transport)
        assertTrue(facts.snapshot().pollInFlight)
        facts.success(foreground, 2000)
        facts.finish(foreground, 2000, null)
        assertFalse(facts.snapshot().pollInFlight)
        assertNull(facts.snapshot().lastFailure)
        assertEquals(2000L, facts.snapshot().lastSuccessAt)
    }
    @Test fun trustCtasRequireKeysAndSeparateChangedKeyConfirmation() {
        assertEquals(ContactCta.ScanKeys, contactCta(Dialog("P", "requested")))
        assertEquals(ContactCta.Accept, contactCta(Dialog("P", "requested", hasKeys = true)))
        assertEquals(ContactCta.Chat, contactCta(Dialog("P", "accepted", hasKeys = true)))
        assertEquals(ContactCta.VerifyChanged, contactCta(Dialog("P", "accepted", true, true)))
        assertTrue(trustLabel(Dialog("P", "accepted", true, true)).contains("СТОП"))
        assertEquals(ContactCta.Blocked, contactCta(Dialog("P", "blocked", true, true)))
    }
    @Test fun asyncGuardRejectsDoubleActionsDuplicateCompletionsAndPausedResults() {
        val guard = UiGuard()
        val first = guard.begin()!!
        assertNull(guard.begin())
        guard.stop()
        val second = guard.begin()!!
        assertFalse(guard.finish(first))
        assertTrue(guard.pending)
        assertTrue(guard.finish(second))
        assertFalse(guard.finish(second))
        assertFalse(guard.accepts(first))
    }
    @Test fun failureRetainsDraftAndConfirmedInsertionConsumesDraftDespiteUnknownStatus() {
        val f = FakeFacade()
        val draft = OutgoingDraft().apply { text = "fixture" }
        val submitted = draft.begin()!!
        assertNull(draft.begin())
        f.sendFailure = DmsgError("fixture failure", ErrorKind.Store)
        draft.finish(runCatching { f.send("P", submitted) }.isSuccess)
        assertEquals("fixture", draft.text)
        assertTrue(f.history.isEmpty())
        f.sendFailure = null
        val retried = draft.begin()!!
        val sent = runCatching { f.send("P", retried) }
        // Status can be unknown independently of the committed message.
        assertEquals("Статус пока неизвестен", deliveryLabel(null))
        draft.finish(sent.isSuccess)
        assertEquals("", draft.text)
        assertEquals(1, f.history.size)
    }
    @Test fun asynchronousCompletionNeverClearsAReplacementDraft() {
        val draft = OutgoingDraft().apply { text = "first" }
        assertEquals("first", draft.begin())
        draft.text = "next"
        draft.finish(true)
        assertEquals("next", draft.text)
    }
    @Test fun failedNetworkAfterCommitUsesDurableIdWithoutResubmittingPlaintext() {
        val f = FakeFacade().apply { failAfterInsert = true }
        val result = sendTextSafely(f, "P", "fixture") as TextSendOutcome.Saved
        assertTrue(result.recoveredAfterError)
        assertEquals(f.history.single().messageIdHex, result.messageId)
        assertEquals(uniffi.dmsg_core.DeliveryState.QUEUED, f.messageStatus(result.messageId))
        assertEquals(1, f.history.size)
    }
    @Test fun unresolvedLocalReadBlocksGuessingAndCanBeReconciledLater() {
        val real = FakeFacade().apply { failAfterInsert = true }
        val failing = object : DmsgFacade by real {
            private var reads = 0
            override fun historyPage(contactId: String, beforeLocalId: Long?, limit: Int): uniffi.dmsg_core.HistoryPage {
                if (++reads > 1) throw DmsgError("fixture storage failure", ErrorKind.Store)
                return real.historyPage(contactId, beforeLocalId, limit)
            }
        }
        val unknown = sendTextSafely(failing, "P", "fixture") as TextSendOutcome.Uncertain
        val resolved = reconcileTextSend(real, "P", unknown.baseline, unknown.text) as TextSendOutcome.Saved
        assertEquals(real.history.single().messageIdHex, resolved.messageId)
        assertEquals(1, real.history.size)
    }
    @Test fun crossScreenPendingGuardPreservesProofAcrossLaterSends() {
        val real = FakeFacade().apply { failAfterInsert = true }
        val failing = object : DmsgFacade by real {
            private var reads = 0
            override fun historyPage(contactId: String, beforeLocalId: Long?, limit: Int): uniffi.dmsg_core.HistoryPage {
                if (++reads > 1) throw DmsgError("fixture storage failure", ErrorKind.Store)
                return real.historyPage(contactId, beforeLocalId, limit)
            }
        }
        val attempt = TextSendCoordinator.send(failing, "P", "fixture") as TextSendOutcome.Uncertain
        assertEquals(attempt, TextSendCoordinator.pendingFor("P"))
        assertTrue(TextSendCoordinator.send(failing, "P", "fixture") is TextSendOutcome.NotSaved)
        assertEquals(1, real.history.size)
        val first = TextSendCoordinator.reconcile(real, "P", attempt) as TextSendOutcome.Saved
        TextSendCoordinator.send(real, "P", "fixture")
        assertEquals(2, real.history.size)
        assertEquals(first, TextSendCoordinator.reconcile(real, "P", attempt))
    }
}
