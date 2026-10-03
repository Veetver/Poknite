package org.poknite

import android.app.NotificationManager
import android.content.Intent
import android.test.InstrumentationTestCase
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.MediaType.Companion.toMediaType
import org.json.JSONObject
import org.json.JSONArray

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
        val device = JSONObject(app.request("/v2/devices/enroll", "POST", JSONObject().put("invitation", invitation).put("device_name", "Android integration"), address, false))
        val sender = JSONObject(app.request("/v2/devices/enroll", "POST", JSONObject().put("invitation", senderInvite).put("device_name", "Integration sender"), address, false))
        app.store.resetAccount(); app.settings.endpoint = address; app.settings.userId = device.getLong("user_id"); app.settings.deviceId = device.getLong("device_id"); app.settings.token = device.getString("token"); app.settings.enabled = true
        assertFalse(app.settings.token == instrumentation.targetContext.getSharedPreferences("settings", 0).getString("token", ""))
        val activity = instrumentation.startActivitySync(Intent(instrumentation.targetContext, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        try {
            until { app.online && app.store.channels().isNotEmpty() }
            val channel = app.store.channels().first().id
            // Closing the UI must not terminate the foreground connection.
            instrumentation.runOnMainSync { activity.finish() }
            val members = JSONArray(app.request("/v2/conversations/$channel/e2ee-members"))
            val code = app.e2ee.createKey(channel, members)
            val senderSettings = Settings(instrumentation.targetContext,"delivery-e2ee-sender")
            senderSettings.endpoint = address; senderSettings.userId = sender.getLong("user_id"); senderSettings.deviceId = sender.getLong("device_id")
            val senderStore = LocalStore(instrumentation.targetContext,"delivery-e2ee-sender.db")
            val id = try {
                val crypto = E2ee(senderSettings,senderStore)
                crypto.importKey(channel,members,code)
                val draft = senderStore.saveDraft(channel,"Доставка при закрытом экране приложения")
                val body = crypto.seal(draft,members)
                val r = app.http.newCall(Request.Builder().url("$address/v2/conversations/$channel/messages").header("Authorization", "Bearer ${sender.getString("token")}").post(body.toString().toRequestBody("application/json".toMediaType())).build()).execute()
                r.use {
                    assertTrue(it.isSuccessful)
                    val plain = crypto.decodeWire(JSONObject(it.body!!.string()))
                    assertTrue(plain.verified); assertEquals(draft.text,plain.text)
                    plain.id
                }
            } finally {
                senderStore.close(); instrumentation.targetContext.deleteDatabase("delivery-e2ee-sender.db")
                instrumentation.targetContext.getSharedPreferences("delivery-e2ee-sender",0).edit().clear().commit()
            }
            until { app.store.history(channel).any { it.id == id } }
            until { app.store.pending(false).isEmpty() }
            assertTrue(instrumentation.targetContext.getSystemService(NotificationManager::class.java).activeNotifications.any { it.tag == id })
            app.request("/v2/devices/${device.getLong("device_id")}", "DELETE")
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
