package org.poknite

import android.app.NotificationManager
import android.content.Intent
import android.test.InstrumentationTestCase
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.MediaType.Companion.toMediaType
import org.json.JSONObject

/** Optional live-server integration. Supply two fresh invitations for different users. */
@Suppress("DEPRECATION")
class DeliveryTest : InstrumentationTestCase() {
    fun testLiveServerBackgroundDeliveryAndRevocation() {
        val endpoint = TestRunner.args.getString("server") ?: return
        assertTrue(BuildConfig.DEBUG)
        val app = instrumentation.targetContext.applicationContext as PokniteApp
        val address = canonicalEndpoint(endpoint, true)
        val invitation = TestRunner.args.getString("invitation") ?: error("Need invitation")
        val senderInvite = TestRunner.args.getString("senderInvitation") ?: error("Need senderInvitation")
        val device = JSONObject(app.request("/v1/devices/enroll", "POST", JSONObject().put("invitation", invitation).put("device_name", "Android integration"), address, false))
        val sender = JSONObject(app.request("/v1/devices/enroll", "POST", JSONObject().put("invitation", senderInvite).put("device_name", "Integration sender"), address, false))
        app.store.resetAccount(); app.settings.endpoint = address; app.settings.userId = device.getLong("user_id"); app.settings.deviceId = device.getLong("device_id"); app.settings.token = device.getString("token"); app.settings.enabled = true
        assertFalse(app.settings.token == instrumentation.targetContext.getSharedPreferences("settings", 0).getString("token", ""))
        val activity = instrumentation.startActivitySync(Intent(instrumentation.targetContext, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        try {
            until { app.online && app.store.channels().isNotEmpty() }
            val channel = app.store.channels().first().id
            // Closing the UI must not terminate the foreground connection.
            instrumentation.runOnMainSync { activity.finish() }
            val body = JSONObject().put("client_message_id", newClientId()).put("text", "Доставка при закрытом экране приложения")
            val r = app.http.newCall(Request.Builder().url("$address/v1/channels/$channel/messages").header("Authorization", "Bearer ${sender.getString("token")}").post(body.toString().toRequestBody("application/json".toMediaType())).build()).execute()
            val id = r.use { assertTrue(it.isSuccessful); JSONObject(it.body!!.string()).getString("id") }
            until { app.store.history(channel).any { it.id == id } }
            until { app.store.pending(false).isEmpty() }
            assertTrue(instrumentation.targetContext.getSystemService(NotificationManager::class.java).activeNotifications.any { it.tag == id })
            app.request("/v1/devices/${device.getLong("device_id")}", "DELETE")
            until { !app.settings.enabled }
            assertFalse(app.online)
        } finally {
            instrumentation.runOnMainSync { activity.finish(); instrumentation.targetContext.stopService(Intent(instrumentation.targetContext, ConnectionService::class.java)) }
            app.settings.enabled = false
        }
    }
    private fun until(check: () -> Boolean) {
        val end = System.currentTimeMillis() + 60_000
        while (!check()) { if (System.currentTimeMillis() >= end) error("Timed out: live Android delivery"); Thread.sleep(200) }
    }
}
