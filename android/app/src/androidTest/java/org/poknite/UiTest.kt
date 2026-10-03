package org.poknite

import android.content.Intent
import android.test.InstrumentationTestCase
import android.view.View
import android.view.ViewGroup
import android.widget.EditText
import android.widget.Spinner
import org.json.JSONArray

@Suppress("DEPRECATION")
class UiTest : InstrumentationTestCase() {
    fun testDraftSurvivesActivityRecreation() {
        assertTrue(BuildConfig.DEBUG)
        val app = instrumentation.targetContext.applicationContext as PokniteApp
        app.settings.enabled = false; app.settings.endpoint = "https://example.org"; app.settings.token = "0".repeat(64)
        app.settings.selectedConversation = 0
        app.store.resetAccount(); app.store.updateChannels(JSONArray("[{\"id\":1,\"name\":\"Общий\"}]"))
        val draft = app.store.saveDraft(1, "Не терять при повороте экрана")
        val activity = instrumentation.startActivitySync(Intent(instrumentation.targetContext, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        val monitor = instrumentation.addMonitor(MainActivity::class.java.name, null, false)
        try {
            instrumentation.waitForIdleSync()
            instrumentation.runOnMainSync { activity.recreate() }
            val recreated = monitor.waitForActivityWithTimeout(30_000)
            assertNotNull(recreated)
            instrumentation.waitForIdleSync()
            assertEquals(draft, app.store.draft(1))
            instrumentation.runOnMainSync { recreated?.finish() }
        } finally {
            instrumentation.removeMonitor(monitor)
            instrumentation.runOnMainSync { activity.finish() }
            app.settings.token = ""; app.settings.selectedConversation = 0; app.store.resetAccount()
        }
    }
    private fun descendants(view: View): List<View> = listOf(view) +
        if (view is ViewGroup) (0 until view.childCount).flatMap { descendants(view.getChildAt(it)) } else emptyList()

    fun testSelectedConversationAndDraftSurviveFreshLaunch() {
        assertTrue(BuildConfig.DEBUG)
        val app = instrumentation.targetContext.applicationContext as PokniteApp
        app.settings.enabled = false; app.settings.endpoint = "https://example.org"; app.settings.token = "0".repeat(64)
        app.settings.selectedConversation = 0
        app.store.resetAccount()
        app.store.updateChannels(JSONArray("""[{"id":1,"name":"Общий","actions":["read","send"]},{"id":7,"name":"Рабочий","actions":["read","send"]}]"""))
        app.store.saveDraft(1, "Черновик общего канала")
        app.store.saveDraft(7, "Черновик рабочего канала")
        val intent = Intent(instrumentation.targetContext, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        var activity = instrumentation.startActivitySync(intent)
        try {
            instrumentation.waitForIdleSync()
            instrumentation.runOnMainSync {
                descendants(activity.window.decorView).filterIsInstance<Spinner>().single().setSelection(1)
            }
            instrumentation.waitForIdleSync()
            instrumentation.runOnMainSync {
                val views = descendants(activity.window.decorView)
                val editor = views.filterIsInstance<EditText>().single()
                assertEquals("Черновик рабочего канала", editor.text.toString())
                editor.setText("Изменённый рабочий черновик")
                views.filterIsInstance<Spinner>().single().setSelection(0)
            }
            instrumentation.waitForIdleSync()
            instrumentation.runOnMainSync {
                val views = descendants(activity.window.decorView)
                assertEquals("Черновик общего канала", views.filterIsInstance<EditText>().single().text.toString())
                views.filterIsInstance<Spinner>().single().setSelection(1)
            }
            instrumentation.waitForIdleSync()
            assertEquals(7L, Settings(instrumentation.targetContext).selectedConversation)
            instrumentation.runOnMainSync { activity.finish() }
            instrumentation.waitForIdleSync()
            activity = instrumentation.startActivitySync(intent)
            instrumentation.waitForIdleSync()
            instrumentation.runOnMainSync {
                val views = descendants(activity.window.decorView)
                assertEquals(1, views.filterIsInstance<Spinner>().single().selectedItemPosition)
                assertEquals("Изменённый рабочий черновик", views.filterIsInstance<EditText>().single().text.toString())
            }
            // A removed channel falls back to an available one on the next launch.
            instrumentation.runOnMainSync { activity.finish() }
            instrumentation.waitForIdleSync()
            app.store.updateChannels(JSONArray("""[{"id":1,"name":"Общий","actions":["read","send"]}]"""))
            activity = instrumentation.startActivitySync(intent)
            instrumentation.waitForIdleSync()
            instrumentation.runOnMainSync {
                assertEquals("Черновик общего канала", descendants(activity.window.decorView).filterIsInstance<EditText>().single().text.toString())
            }
            assertEquals(1L, Settings(instrumentation.targetContext).selectedConversation)
        } finally {
            instrumentation.runOnMainSync { activity.finish() }
            app.settings.token = ""; app.settings.selectedConversation = 0; app.store.resetAccount()
        }
    }
}
