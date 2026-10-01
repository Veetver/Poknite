package org.poknite

import android.content.Intent
import android.test.InstrumentationTestCase
import org.json.JSONArray

@Suppress("DEPRECATION")
class UiTest : InstrumentationTestCase() {
    fun testDraftSurvivesActivityRecreation() {
        assertTrue(BuildConfig.DEBUG)
        val app = instrumentation.targetContext.applicationContext as PokniteApp
        app.settings.enabled = false; app.settings.endpoint = "https://example.org"; app.settings.token = "0".repeat(64)
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
            app.settings.token = ""; app.store.resetAccount()
        }
    }
}
