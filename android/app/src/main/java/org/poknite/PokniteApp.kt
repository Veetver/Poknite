package org.poknite

import android.app.Application
import android.content.Intent
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.MediaType.Companion.toMediaType
import org.json.JSONObject
import java.io.IOException
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

class ApiFailure(val statusCode: Int, message: String) : IOException(message)
class PokniteApp : Application() {
    @Volatile var online = false
    lateinit var settings: Settings
    lateinit var store: LocalStore
    val io = Executors.newSingleThreadExecutor()
    val http = OkHttpClient.Builder().followRedirects(false).followSslRedirects(false).connectTimeout(15, TimeUnit.SECONDS).readTimeout(20, TimeUnit.SECONDS).writeTimeout(20, TimeUnit.SECONDS).pingInterval(120, TimeUnit.SECONDS).build()
    override fun onCreate() { super.onCreate(); settings = Settings(this); store = LocalStore(this) }
    fun changed() { sendBroadcast(Intent("org.poknite.CHANGED").setPackage(packageName), "org.poknite.INTERNAL") }
    fun request(path: String, method: String = "GET", body: JSONObject? = null, endpoint: String = settings.endpoint, authenticated: Boolean = true): String {
        canonicalEndpoint(endpoint, BuildConfig.DEBUG)
        val b = Request.Builder().url(endpoint + path)
        if (authenticated) b.header("Authorization", "Bearer ${settings.token}")
        if (method != "GET") b.method(method, if (method == "DELETE") null else (body ?: JSONObject()).toString().toRequestBody("application/json; charset=utf-8".toMediaType()))
        http.newCall(b.build()).execute().use { r ->
            val source = r.body?.source()
            if (source != null && source.request(32769)) throw IOException("Ответ сервера слишком большой")
            val text = source?.readUtf8() ?: ""
            if (!r.isSuccessful) throw ApiFailure(r.code, runCatching { JSONObject(text).getString("message") }.getOrDefault("Ошибка сервера ${r.code}"))
            return text
        }
    }
}
