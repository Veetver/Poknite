package org.poknite

import okhttp3.HttpUrl.Companion.toHttpUrl
import org.json.JSONObject
import java.util.UUID
import kotlin.math.min

data class Channel(val id: Long, val name: String)
data class WireMessage(val id: String, val seq: Long, val channelId: Long, val senderId: Long, val senderName: String, val text: String, val createdAt: Long, val expiresAt: Long) {
    companion object {
        fun parse(j: JSONObject): WireMessage = WireMessage(j.getString("id"), j.getLong("seq"), j.getLong("channel_id"), j.getLong("sender_id"), j.getString("sender_name"), j.getString("text"), j.getLong("created_at"), j.getLong("expires_at")).also {
            require(it.seq > 0 && it.channelId > 0 && validText(it.text) && it.id.length <= 64 && it.senderName.length <= 128)
        }
    }
}
data class Draft(val channelId: Long, val text: String, val clientId: String)
fun validText(text: String): Boolean = text.isNotBlank() && text.toByteArray(Charsets.UTF_8).size <= 4096 && text.none { (it.code < 32 || it.code in 127..159) && it !in "\n\r\t" }
fun canonicalEndpoint(raw: String, allowDebugLoopback: Boolean = false): String {
    val u = raw.trim().toHttpUrl()
    require(u.username.isEmpty() && u.password.isEmpty() && u.query == null && u.fragment == null && u.encodedPath == "/") { "Укажите адрес сервера без пути, пароля и параметров" }
    require(u.isHttps || allowDebugLoopback && u.host in setOf("localhost", "127.0.0.1", "::1", "10.0.2.2")) { "Нужен HTTPS-адрес сервера" }
    return u.toString().trimEnd('/')
}
fun newClientId(): String = UUID.randomUUID().toString()
fun retryDelay(attempt: Int, random: Double = Math.random()): Long = ((1000L shl min(attempt, 6)).coerceAtMost(60_000) * (0.8 + random.coerceIn(0.0, 1.0) * 0.4)).toLong()
