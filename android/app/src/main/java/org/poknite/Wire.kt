package org.poknite

import okhttp3.HttpUrl.Companion.toHttpUrl
import org.json.JSONObject
import org.json.JSONArray
import java.util.UUID
import kotlin.math.min

data class Channel(val id: Long, val name: String, val kind: String = "channel", val closed: Boolean = false, val actions: List<String> = emptyList())
data class WireMessage(val id: String, val seq: Long, val channelId: Long, val senderId: Long, val senderName: String, val text: String, val createdAt: Long, val expiresAt: Long, val mentions: String = "[]",val senderColor: String = "#808080", val sealed: String? = null, val verified: Boolean = true) {
    companion object {
        fun parse(j: JSONObject): WireMessage = WireMessage(j.getString("id"), j.getLong("seq"), j.getLong("channel_id"), j.getLong("sender_id"), j.getString("sender_name"), j.getString("text"), j.getLong("created_at"), j.getLong("expires_at"), j.optJSONArray("mentions")?.toString() ?: "[]",j.optString("sender_color","#808080")).also {
            require(it.seq > 0 && it.channelId > 0 && validText(it.text) && it.id.length <= 64 && it.senderName.length <= 128)
        }
    }
}
data class Draft(val channelId: Long, val text: String, val clientId: String, val mentions: String = "[]")
fun validText(text: String): Boolean = text.isNotBlank() && text.toByteArray(Charsets.UTF_8).size <= 4096 && text.none { (it.code < 32 || it.code in 127..159) && it !in "\n\r\t" }
fun canonicalEndpoint(raw: String, allowDebugLoopback: Boolean = false): String {
    val u = raw.trim().toHttpUrl()
    require(u.username.isEmpty() && u.password.isEmpty() && u.query == null && u.fragment == null && u.encodedPath == "/") { "Укажите адрес сервера без пути, пароля и параметров" }
    require(u.isHttps || allowDebugLoopback && u.host in setOf("localhost", "127.0.0.1", "::1", "10.0.2.2")) { "Нужен HTTPS-адрес сервера" }
    return u.toString().trimEnd('/')
}
fun newClientId(): String = UUID.randomUUID().toString()
fun retryDelay(attempt: Int, random: Double = Math.random()): Long = ((1000L shl min(attempt, 6)).coerceAtMost(60_000) * (0.8 + random.coerceIn(0.0, 1.0) * 0.4)).toLong()

fun adjustMentions(old: String, new: String, encoded: String): String {
    val a = old.codePoints().toArray(); val b = new.codePoints().toArray()
    var prefix = 0; while (prefix < min(a.size,b.size) && a[prefix] == b[prefix]) prefix++
    var suffix = 0; while (suffix < min(a.size-prefix,b.size-prefix) && a[a.size-1-suffix] == b[b.size-1-suffix]) suffix++
    val result = JSONArray(); val mentions = JSONArray(encoded)
    for (i in 0 until mentions.length()) {
        val m = mentions.getJSONObject(i); val start = m.getInt("start"); val end = m.getInt("end")
        if (end <= prefix) result.put(m)
        else if (start >= a.size-suffix) result.put(JSONObject().put("user_id",m.getLong("user_id")).put("start",start+b.size-a.size).put("end",end+b.size-a.size))
    }
    return result.toString()
}

fun rgbColor(raw: String): String {
    if(raw.matches(Regex("#[0-9a-fA-F]{6}")))return raw.lowercase()
    val rgb=raw.split(',').map { it.trim().toInt() };require(rgb.size==3 && rgb.all { it in 0..255 }) { "RGB: три числа от 0 до 255 через запятую" }
    return "#%02x%02x%02x".format(rgb[0],rgb[1],rgb[2])
}
fun rgbString(color: String): String {val c=color.removePrefix("#").toIntOrNull(16) ?: 0x808080;return "${c shr 16 and 255},${c shr 8 and 255},${c and 255}"}
fun coloredNickname(name: String,color: String): android.text.SpannableString = android.text.SpannableString(name).apply {setSpan(android.text.style.ForegroundColorSpan(runCatching { android.graphics.Color.parseColor(color) }.getOrDefault(android.graphics.Color.GRAY)),0,length,android.text.Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)}
