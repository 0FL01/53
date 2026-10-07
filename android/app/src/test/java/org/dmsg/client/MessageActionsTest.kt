package org.dmsg.client

import org.junit.Assert.*
import org.junit.Test
import uniffi.dmsg_core.DeleteScope
import uniffi.dmsg_core.DeliveryState
import uniffi.dmsg_core.HistoryMessage
import uniffi.dmsg_core.MessageDirection

class MessageActionsTest {
    private val contact = Dialog("P", "accepted", hasKeys = true)
    private fun row(id: Long = 1, state: DeliveryState = DeliveryState.ACCEPTED) = HistoryMessage(
        localId = id, messageIdHex = id.toString(16).padStart(32, '0'), contactId = "P", direction = MessageDirection.OUTGOING,
        text = "original", localTimestampMs = 1000, deliveryState = state, serverSeq = 10, serverTimestampMs = 2000,
        revision = 0uL, hiddenSelf = false, deletedAll = false, changeDeliveryState = null,
        kind = uniffi.dmsg_core.MessageKind.TEXT, voice = null)
    private fun facade() = FakeFacade().apply { dialogs.add(contact); history.add(row()) }

    @Test fun onlyVisibleOwnConfirmedMessagesOfferRemoteActionsAndSelfIsDefault() {
        val own = row()
        assertTrue(canChangeMessage(own, contact))
        assertTrue(canChangeMessage(own.copy(deliveryState = DeliveryState.DELIVERED), contact))
        assertEquals(listOf(DeleteScope.SELF_ONLY, DeleteScope.EVERYONE), deleteScopes(own, contact))
        for (blocked in listOf(contact.copy(state = "blocked"), contact.copy(identityMismatch = true), null)) {
            assertFalse(canChangeMessage(own, blocked))
            assertEquals(listOf(DeleteScope.SELF_ONLY), deleteScopes(own, blocked))
        }
        assertEquals(listOf(DeleteScope.SELF_ONLY), deleteScopes(row(state = DeliveryState.QUEUED), contact))
        for (unavailable in listOf(own.copy(direction = MessageDirection.INCOMING), own.copy(hiddenSelf = true), own.copy(deletedAll = true))) {
            assertFalse(canChangeMessage(unavailable, contact)); assertTrue(deleteScopes(unavailable, contact).isEmpty())
        }
        val f = facade()
        // Opening/cancelling the selection has no writer or delivery effect.
        assertEquals(DeleteScope.SELF_ONLY, deleteScopes(f.history.single(), contact).first())
        assertEquals(0, f.mutationCalls); assertEquals(0, f.retryCalls)
    }

    @Test fun editCancelAndSavedRestoreNormalDraftAndDoNotConsumeIt() {
        val composer = MessageComposer().apply { text = "normal draft" }
        assertTrue(composer.start(row()))
        composer.text = "edit draft"
        assertEquals("normal draft", composer.normal.text)
        assertFalse(composer.start(row(2)))
        composer.cancel()
        assertNull(composer.edit); assertEquals("normal draft", composer.text)
        composer.start(row()); composer.text = "saved edit"
        composer.saved(2)
        assertNotNull(composer.edit)
        composer.saved(1)
        assertNull(composer.edit); assertEquals("normal draft", composer.text)
    }

    @Test fun conflictKeepsTextAndRefreshRebasesOnlyAfterExplicitConflict() {
        val composer = MessageComposer().apply { normal.text = "normal"; start(row()); text = "my edit" }
        val changed = row().copy(revision = 1uL, text = "other edit")
        composer.refresh(changed)
        assertEquals(0uL, composer.edit!!.expectedRevision)
        val f = facade().apply { history[0] = changed }
        val failed = TextSendCoordinator.mutate(f, "P", 1, MessageActionCommand.Edit(0uL, composer.text)) as MessageActionOutcome.NotSaved
        assertEquals(ErrorKind.MessageChanged, (failed.error as DmsgError).kind)
        assertEquals(1, f.mutationCalls); assertTrue(f.controls.isEmpty()); assertEquals(0, f.retryCalls)
        composer.refresh(f.historyMessage("P", 1), rebase = true)
        assertEquals("my edit", composer.text)
        assertEquals("other edit", composer.edit!!.baseline)
        assertEquals(1uL, composer.edit!!.expectedRevision)
        assertEquals(1, f.mutationCalls) // refresh never auto-submits
        assertTrue(TextSendCoordinator.mutate(f, "P", 1, MessageActionCommand.Edit(1uL, composer.text)) is MessageActionOutcome.Saved)
        assertEquals(2uL, f.history.single().revision)
    }

    @Test fun hiddenUnavailableTargetRemainsEditModeWithDraftAndNoNewSend() {
        val composer = MessageComposer().apply { normal.text = "normal"; start(row()); text = "my edit" }
        composer.refresh(row().copy(hiddenSelf = true, text = ""))
        assertTrue(composer.edit!!.unavailable)
        assertEquals("my edit", composer.text); assertEquals("normal", composer.normal.text)
        assertEquals(1L, composer.edit!!.localId)
        composer.unavailable()
        assertNotNull(composer.edit)
        composer.cancel()
        assertEquals("normal", composer.text)
    }

    @Test fun sameTextStillUsesNativeRevisionCasAndDoesNotQueueAnEdit() {
        val f = facade()
        val command = MessageActionCommand.Edit(0uL, "original")
        val saved = TextSendCoordinator.mutate(f, "P", 1, command) as MessageActionOutcome.Saved
        assertEquals(R.string.edit_unchanged, actionSavedRes(command, saved.row))
        assertEquals(1, f.mutationCalls)
        assertTrue(f.controls.isEmpty())
        assertEquals(0, f.retryCalls)
        val failed = TextSendCoordinator.mutate(f, "P", 1, MessageActionCommand.Edit(1uL, "original")) as MessageActionOutcome.NotSaved
        assertEquals(ErrorKind.MessageChanged, (failed.error as DmsgError).kind)
    }

    @Test fun localCommitIsSavedEvenWhenNetworkAndLaterReadsFail() {
        val real = facade().apply { retryFailure = DmsgError(R.string.error_transport, ErrorKind.Transport) }
        val f = object : DmsgFacade by real {
            private var reads = 0
            override fun historyMessage(contactId: String, localId: Long): HistoryMessage {
                if (++reads > 1) throw DmsgError(R.string.error_store, ErrorKind.Store)
                return real.historyMessage(contactId, localId)
            }
        }
        val saved = TextSendCoordinator.mutate(f, "P", 1, MessageActionCommand.Edit(0uL, "edited")) as MessageActionOutcome.Saved
        assertNotNull(saved.networkError)
        assertEquals("edited", saved.row.text)
        assertEquals(DeliveryState.ACCEPTED, saved.row.deliveryState)
        assertEquals(DeliveryState.QUEUED, saved.row.changeDeliveryState)
        assertEquals(10L, saved.row.serverSeq); assertEquals(2000L, saved.row.serverTimestampMs)
        assertEquals(1, real.mutationCalls); assertEquals(1, real.retryCalls)
        assertEquals(R.string.edit_saved, actionSavedRes(MessageActionCommand.Edit(0uL, "edited"), saved.row))
    }

    @Test fun queuedSelfHideIsLocalDespiteBlockedContactAndNeverRetriesOrCancelsOriginal() {
        val f = facade().apply { history[0] = row(state = DeliveryState.QUEUED); dialogs[0] = contact.copy(state = "blocked") }
        val command = MessageActionCommand.Delete(DeleteScope.SELF_ONLY)
        val saved = TextSendCoordinator.mutate(f, "P", 1, command) as MessageActionOutcome.Saved
        assertTrue(saved.row.hiddenSelf); assertFalse(saved.row.deletedAll)
        assertEquals("", saved.row.text); assertTrue(f.controls.isEmpty()); assertEquals(0, f.retryCalls)
        assertEquals(DeliveryState.QUEUED, f.messageStatus(saved.row.messageIdHex))
        assertEquals(R.string.self_hidden_queued, actionSavedRes(command, saved.row))
    }

    @Test fun deletionOutboxUsesOwnMidAndStatusDistinctFromOriginal() {
        val f = facade().apply { retryFailure = DmsgError(R.string.error_transport, ErrorKind.Transport) }
        val command = MessageActionCommand.Delete(DeleteScope.EVERYONE)
        val saved = TextSendCoordinator.mutate(f, "P", 1, command) as MessageActionOutcome.Saved
        assertTrue(saved.row.deletedAll); assertNotNull(saved.networkError)
        assertEquals(R.string.delete_saved, actionSavedRes(command, saved.row))
        val event = f.outbox(0, 50).first.single()
        assertEquals("delete", event.kind); assertEquals(R.string.outbox_delete, outboxKindRes(event.kind))
        assertEquals(DeliveryState.QUEUED, f.messageStatus(event.mid))
        assertEquals(DeliveryState.ACCEPTED, f.messageStatus(saved.row.messageIdHex))
        f.controlStates[event.mid] = DeliveryState.DELIVERED
        assertEquals(DeliveryState.DELIVERED, f.messageStatus(event.mid))
        assertEquals(DeliveryState.ACCEPTED, f.messageStatus(saved.row.messageIdHex))
    }

    @Test fun unresolvedSendBlocksEvenLocalSelfHideOnTheWorker() {
        val real = facade().apply { failAfterInsert = true }
        val failing = object : DmsgFacade by real {
            private var reads = 0
            override fun historyPage(contactId: String, beforeLocalId: Long?, limit: Int): uniffi.dmsg_core.HistoryPage {
                if (++reads > 1) throw DmsgError(R.string.error_store, ErrorKind.Store)
                return real.historyPage(contactId, beforeLocalId, limit)
            }
        }
        val attempt = TextSendCoordinator.send(failing, "P", "send") as TextSendOutcome.Uncertain
        try {
            val blocked = TextSendCoordinator.mutate(failing, "P", 1, MessageActionCommand.Delete(DeleteScope.SELF_ONLY)) as MessageActionOutcome.NotSaved
            assertEquals(R.string.error_pending_send, humanErrorRes(blocked.error))
            assertEquals(0, real.mutationCalls); assertFalse(real.history.first().hiddenSelf)
        } finally { TextSendCoordinator.reconcile(real, "P", attempt) }
    }

    @Test fun unresolvedMutationBlocksCrossChatWritesAndRetainsExactProofAfterLaterEdit() {
        val real = facade().apply { failAfterMutation = true }
        val failing = object : DmsgFacade by real {
            private var reads = 0
            override fun historyMessage(contactId: String, localId: Long): HistoryMessage {
                if (++reads > 1) throw DmsgError(R.string.error_store, ErrorKind.Store)
                return real.historyMessage(contactId, localId)
            }
        }
        val unknown = TextSendCoordinator.mutate(failing, "P", 1, MessageActionCommand.Edit(0uL, "edited")) as MessageActionOutcome.Uncertain
        try {
            assertEquals(unknown.attempt, TextSendCoordinator.pendingActionFor("P"))
            assertTrue(TextSendCoordinator.send(failing, "OTHER", "new") is TextSendOutcome.NotSaved)
            assertTrue(TextSendCoordinator.mutate(failing, "P", 1, MessageActionCommand.Delete(DeleteScope.SELF_ONLY)) is MessageActionOutcome.NotSaved)
            assertEquals(1, real.mutationCalls); assertEquals(1, real.history.size)
            val restored = MessageComposer().apply { restore(unknown.attempt) }
            assertEquals("edited", restored.text); assertEquals(0uL, restored.edit!!.expectedRevision)
        } finally { TextSendCoordinator.reconcileAction(real, unknown.attempt) }
        val proof = TextSendCoordinator.reconcileAction(real, unknown.attempt) as MessageActionOutcome.Saved
        assertEquals(1uL, proof.row.revision)
        assertNull(TextSendCoordinator.pendingActionFor("P"))
        real.failAfterMutation = false
        TextSendCoordinator.mutate(real, "P", 1, MessageActionCommand.Edit(1uL, "later"))
        assertEquals(2uL, real.history.single().revision)
        assertEquals(proof, TextSendCoordinator.reconcileAction(real, unknown.attempt))
        assertEquals(2, real.mutationCalls)
    }

    @Test fun exactMutationRecoveryDoesNotSearchOtherRowsForMatchingPlaintext() {
        val f = facade().apply { history.add(row(2).copy(revision = 1uL, text = "edited")) }
        val attempt = MessageActionAttempt(row(), MessageActionCommand.Edit(0uL, "edited"), DmsgError(R.string.error_store, ErrorKind.Store))
        assertTrue(reconcileMessageAction(f, attempt) is MessageActionOutcome.NotSaved)
        f.history[0] = row().copy(revision = 2uL, text = "edited")
        val failed = reconcileMessageAction(f, attempt) as MessageActionOutcome.NotSaved
        assertEquals(ErrorKind.MessageChanged, (failed.error as DmsgError).kind)
        assertEquals(0, f.mutationCalls)
    }
}
