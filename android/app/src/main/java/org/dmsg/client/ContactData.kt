package org.dmsg.client

/** Bounded reads through the summary API; no DB access or alias copy in preferences. */
internal fun DmsgFacade.summary(id: String): uniffi.dmsg_core.DialogSummary? {
    var cursor: String? = null
    do {
        val page = dialogsPage(cursor, 100)
        page.rows.firstOrNull { it.contactId == id }?.let { return it }
        cursor = page.nextCursor
    } while (cursor != null)
    return null
}
