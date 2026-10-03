package org.poknite

import android.test.InstrumentationTestCase
import org.json.JSONArray
import org.json.JSONObject

@Suppress("DEPRECATION")
class E2eeTest : InstrumentationTestCase() {
    private lateinit var store:LocalStore
    private lateinit var settings:Settings
    private lateinit var crypto:E2ee
    private lateinit var vector:JSONObject
    override fun setUp() {
        super.setUp()
        val context=instrumentation.targetContext
        context.deleteDatabase("e2ee-test.db");context.getSharedPreferences("e2ee-test",0).edit().clear().commit()
        store=LocalStore(context,"e2ee-test.db");settings=Settings(context,"e2ee-test");settings.endpoint="https://example.test";settings.userId=1;settings.deviceId=1
        crypto=E2ee(settings,store)
        vector=JSONObject(instrumentation.context.assets.open("e2ee.json").bufferedReader().use {it.readText()})
    }
    override fun tearDown() {store.close();instrumentation.targetContext.deleteDatabase("e2ee-test.db");instrumentation.targetContext.getSharedPreferences("e2ee-test",0).edit().clear().commit();super.tearDown()}
    private fun wire(sealed:String)=JSONObject().put("id","server-id").put("seq",1).put("channel_id",1).put("sender_id",1).put("sender_name","Автор").put("sender_color","#808080").put("text",sealed).put("created_at",1).put("expires_at",2).put("mentions",vector.getJSONObject("header").getJSONArray("mentions"))
    fun testIndependentVectorAndProtectedMetadata() {
        assertEquals(vector.getString("members_digest"),E2ee.membersDigest(vector.getJSONArray("members")))
        assertEquals(vector.getString("text"),E2ee.decrypt(E2ee.unb64(vector.getString("key")),E2ee.parse(vector.getString("sealed"))))
        // JSONObject escapes '/' in the protected JSON. JWE authenticates those
        // exact bytes; the fixture contains an independently generated variant.
        assertEquals(vector.getString("sealed_android"),E2ee.encrypt(E2ee.unb64(vector.getString("key")),vector.getJSONObject("header"),vector.getString("text"),E2ee.unb64(vector.getString("nonce"))))
        crypto.importKey(1,vector.getJSONArray("members"),vector.getString("code"))
        val original=wire(vector.getString("sealed"));assertTrue(crypto.decodeWire(original).verified)
        assertEquals(vector.getString("text"),crypto.decodeWire(original).text)
        assertFalse(crypto.decodeWire(JSONObject(original.toString()).put("sender_id",2)).verified)
        assertFalse(crypto.decodeWire(JSONObject(original.toString()).put("channel_id",2)).verified)
        assertFalse(crypto.decodeWire(JSONObject(original.toString()).put("mentions",JSONArray())).verified)
        val parts=vector.getString("sealed").removePrefix("e2ee:").split('.').toMutableList();val tag=E2ee.unb64(parts[4]);tag[0]=(tag[0].toInt() xor 1).toByte();parts[4]=E2ee.b64(tag)
        assertFalse(crypto.decodeWire(wire("e2ee:"+parts.joinToString("."))).verified)
        assertFalse(crypto.decodeWire(wire("Открытое сообщение")).verified)
    }
    fun testMissingKeyHistoryAndStableRetries() {
        val original=wire(vector.getString("sealed"));val unavailable=crypto.decodeWire(original)
        assertFalse(unavailable.verified);store.receive(unavailable,true,2,notify=true);assertTrue(store.pending(true).isEmpty())
        crypto.importKey(1,vector.getJSONArray("members"),vector.getString("code"))
        assertEquals(vector.getString("text"),store.history(1).single().text)
        val draft=store.saveDraft(1,"Секретный текст")
        val first=crypto.seal(draft,vector.getJSONArray("members"));val again=crypto.seal(draft,vector.getJSONArray("members"))
        assertEquals(first.getString("text"),again.getString("text"));assertFalse(first.getString("text").contains(draft.text))
        val encryptedPrefs=instrumentation.targetContext.getSharedPreferences("e2ee-test",0).getString("e2ee_keys","")!!
        assertFalse(encryptedPrefs.contains(vector.getString("key")))
        store.close();store=LocalStore(instrumentation.targetContext,"e2ee-test.db");crypto=E2ee(settings,store)
        assertEquals(vector.getString("text"),crypto.decodeWire(original).text)
    }
    fun testMembershipChangeBlocksStaleKey() {
        crypto.importKey(1,vector.getJSONArray("members"),vector.getString("code"))
        val remaining=JSONArray("[{\"user_id\":1,\"device_id\":1}]")
        val draft=store.saveDraft(1,"Новый текст")
        assertTrue(runCatching {crypto.seal(draft,remaining)}.isFailure)
        assertTrue(runCatching {crypto.importKey(1,remaining,vector.getString("code"))}.isFailure)
        crypto.createKey(1,remaining)
        val encrypted=crypto.seal(store.draft(1)!!,remaining)
        assertEquals("Новый текст",E2ee.decrypt(E2ee.unb64(JSONObject(String(E2ee.unb64(crypto.exportKey(1).removePrefix("poknite-key-v1:")),Charsets.UTF_8)).getString("key")),E2ee.parse(encrypted.getString("text"))))
    }
}
