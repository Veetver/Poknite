package org.poknite

import android.app.Application
import android.content.Intent
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.MediaType.Companion.toMediaType
import org.json.JSONObject
import java.io.IOException
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.net.InetAddress
import java.net.Proxy
import java.util.concurrent.CountDownLatch
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okhttp3.Response

class ApiFailure(val statusCode: Int, message: String) : IOException(message)
class PokniteApp : Application() {
    @Volatile var online = false
    lateinit var settings: Settings
    lateinit var store: LocalStore
    lateinit var e2ee: E2ee
    val io = Executors.newSingleThreadExecutor()
    val http = OkHttpClient.Builder().followRedirects(false).followSslRedirects(false).connectTimeout(15, TimeUnit.SECONDS).readTimeout(20, TimeUnit.SECONDS).writeTimeout(20, TimeUnit.SECONDS).pingInterval(120, TimeUnit.SECONDS).build()
    override fun onCreate() { super.onCreate(); settings = Settings(this); store = LocalStore(this); e2ee = E2ee(settings,store) }
    fun changed() { sendBroadcast(Intent("org.poknite.CHANGED").setPackage(packageName), "org.poknite.INTERNAL") }
    fun request(path: String, method: String = "GET", body: JSONObject? = null, endpoint: String = settings.endpoint, authenticated: Boolean = true): String {
        if(method=="GET" && catalogPath(path)) return catalog { offset -> request("$path?offset=$offset",endpoint=endpoint,authenticated=authenticated) }
        require(!path.startsWith("/v2/admin")) { "Управление требует отдельного HTTPS-адреса" }
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
    fun adminRequest(path: String, method: String = "GET", body: Any? = null): String {
        if(method=="GET" && catalogPath(path)) return catalog { offset -> adminRequest("$path?offset=$offset") }
        require(path.startsWith("/v2/admin/"))
        val endpoint=canonicalEndpoint(settings.managementEndpoint)
        val raw=settings.managementIp.trim()
        val client=if(raw.isEmpty()) http.newBuilder().proxy(Proxy.NO_PROXY).build() else {
            require(raw.matches(Regex("[0-9.]+|[0-9a-fA-F:]+"))) { "Укажите числовой IP подключения" }
            val ip=InetAddress.getByName(raw)
            val host=endpoint.toHttpUrl().host
            http.newBuilder().proxy(Proxy.NO_PROXY).dns(object:okhttp3.Dns { override fun lookup(hostname:String):List<InetAddress> = if(hostname==host) listOf(ip) else okhttp3.Dns.SYSTEM.lookup(hostname) }).build()
        }
        val b=Request.Builder().url(endpoint+path).header("Authorization","Bearer ${settings.token}")
        if(method!="GET") b.method(method,if(method=="DELETE") null else (body?.toString() ?: "{}").toRequestBody("application/json; charset=utf-8".toMediaType()))
        try {client.newCall(b.build()).execute().use { r ->
            val source=r.body?.source();if(source!=null && source.request(32769)) throw IOException("Ответ сервера слишком большой")
            val text=source?.readUtf8() ?: ""
            if(!r.isSuccessful) throw ApiFailure(r.code,runCatching { JSONObject(text).getString("message") }.getOrDefault("Ошибка управления ${r.code}"))
            return text
        }} catch(e:ApiFailure){throw e} catch(e:IOException){throw IOException("Управление недоступно из текущей сети: ${e.message}",e)}
    }
    fun diagnostics(raw: String): String {
        val endpoint=canonicalEndpoint(raw,BuildConfig.DEBUG)
        val host=endpoint.toHttpUrl().host
        val lines=mutableListOf<String>()
        lines+=runCatching { "DNS: "+InetAddress.getAllByName(host).joinToString { it.hostAddress ?: "" } }.getOrElse { "DNS: не удалось определить адрес: ${it.message}" }
        lines+=runCatching { request("/healthz",endpoint=endpoint,authenticated=false);"HTTPS: соединение и сертификат проверены" }.getOrElse { "HTTPS: ${it.message}" }
        var legacy=false
        while(true) {
        val ready=CountDownLatch(1);var result="WebSocket: превышено время ожидания";var code=0
        val socket=http.newWebSocket(Request.Builder().url(endpoint.replaceFirst("https:","wss:").replaceFirst("http:","ws:")+(if(legacy) "/v1/stream" else "/v2/stream")).build(),object:WebSocketListener(){
            override fun onOpen(webSocket:WebSocket,response:Response){result="WebSocket: соединение установлено";webSocket.close(1000,null);ready.countDown()}
            override fun onFailure(webSocket:WebSocket,t:Throwable,response:Response?){code=response?.code ?: 0;result=if(response?.code==401) "WebSocket: HTTPS/WSS доступны; для потока нужен токен устройства" else "WebSocket: ${t.message}";ready.countDown()}
        })
        ready.await(12,TimeUnit.SECONDS);socket.cancel()
        if(!legacy && code==404){legacy=true;continue}
        if(legacy)lines+="Сервер использует API v1: требуется совместное обновление сервера и клиентов"
        lines+=result;break
        }
        return lines.joinToString("\n\n")
    }

}

private fun catalogPath(path:String):Boolean = path in setOf("/v2/contacts","/v2/channels","/v2/conversations","/v2/admin/users","/v2/admin/channels","/v2/admin/roles") || path.startsWith("/v2/conversations/") && (path.endsWith("/participants") || path.endsWith("/e2ee-members"))
private fun catalog(page:(Int)->String):String {
    val all=org.json.JSONArray()
    while(true){val rows=org.json.JSONArray(page(all.length()));require(all.length()+rows.length()<=1000) {"Слишком большой каталог сервера"};for(i in 0 until rows.length())all.put(rows.get(i));if(rows.length()<16)break}
    return all.toString()
}
