package org.poknite

import android.test.InstrumentationTestCase
import org.json.JSONArray
import org.json.JSONObject

@Suppress("DEPRECATION")
class StorageTest : InstrumentationTestCase() {
    private lateinit var store: LocalStore
    override fun setUp() {
        super.setUp()
        instrumentation.targetContext.deleteDatabase("storage-test.db")
        store = LocalStore(instrumentation.targetContext, "storage-test.db")
    }
    override fun tearDown() { store.close(); instrumentation.targetContext.deleteDatabase("storage-test.db"); super.tearDown() }
    private fun message(seq: Long, sender: Long = 2, text: String = "текст") = WireMessage("msg-$seq", seq, 1, sender, "Имя", text, 1, 2)
    fun testDurableQueueDedupeAndIndependentTtl() {
        store.updateChannels(JSONArray("[{\"id\":1,\"name\":\"Общий\"}]"))
        assertTrue(store.receive(message(1), true, 1))
        assertFalse(store.receive(message(1), true, 1))
        assertEquals(1L, store.cursor())
        store.close(); store = LocalStore(instrumentation.targetContext, "storage-test.db")
        assertEquals(1, store.pending(true).size)
        assertEquals("текст", store.history(1).single().text)
        assertTrue(store.receive(message(2, 1), false, 1))
        assertEquals(0, store.pending(false).size)
        store.shown(listOf("msg-1")); assertEquals(0, store.pending(true).size)
        store.progress(3); store.progress(2); assertEquals(3L, store.cursor())
        store.progress(0, true); assertEquals(0L, store.cursor())
    }
    fun testHttpPublishMustNotSkipStreamAndDraftRetryKeepsId() {
        store.receive(message(100, 1), false, 1, false)
        assertEquals(0L, store.cursor())
        val draft = store.saveDraft(1, "черновик")
        assertEquals(draft.clientId, store.saveDraft(1, "черновик").clientId)
        val edited = store.saveDraft(1, "новый")
        assertFalse(draft.clientId == edited.clientId)
        store.sentDraft(draft); assertNotNull(store.draft(1))
        store.sentDraft(edited); assertNull(store.draft(1))
    }
    fun testHistoryQuotaAndClear() {
        store.readableDatabase.rawQuery("PRAGMA max_page_count", null).use { assertTrue(it.moveToFirst()); assertEquals(2048, it.getInt(0)) }
        for (i in 1L..1010L) store.receive(message(i, 1, "я".repeat(2048)), true, 1)
        store.readableDatabase.rawQuery("SELECT COUNT(*) FROM messages", null).use { assertTrue(it.moveToFirst()); assertEquals(1000, it.getInt(0)) }
        assertEquals(1010L, store.history(1).first().seq)
        assertTrue(instrumentation.targetContext.getDatabasePath("storage-test.db").length() <= 8 * 1024 * 1024)
        store.clearHistory(); assertTrue(store.history(1).isEmpty()); assertEquals(1010L, store.cursor())
    }
    fun testChannelRefreshPreservesQuietAndWireShape() {
        store.updateChannels(JSONArray("[{\"id\":1,\"name\":\"Общий\"}]")); store.setQuiet(1, true)
        store.updateChannels(JSONArray("[{\"id\":1,\"name\":\"Новое имя\"}]")); assertTrue(store.quiet(1))
        val event = JSONObject("""{"id":"f92cf5f0-a605-4c3d-a49b-104dbf193b16","seq":7,"channel_id":1,"sender_id":2,"sender_name":"Имя","text":"текст","created_at":1,"expires_at":2}""")
        assertEquals(message(7).copy(id = event.getString("id")), WireMessage.parse(event))
        store.resetAccount(); assertTrue(store.channels().isEmpty()); assertEquals(0L, store.cursor())
    }
}
