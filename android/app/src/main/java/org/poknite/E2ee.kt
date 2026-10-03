package org.poknite

import android.util.Base64
import org.json.JSONArray
import org.json.JSONObject
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.security.MessageDigest
import java.security.SecureRandom
import java.util.UUID
import javax.crypto.Cipher
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

/** JWE compact, alg=dir, enc=A256GCM. Secret conversation codes never go to HTTP. */
class E2ee(private val settings: Settings, private val store: LocalStore) {
    private val random=SecureRandom()
    private fun audience()=canonicalEndpoint(settings.endpoint,BuildConfig.DEBUG)
    private fun keys()=JSONObject(settings.secret("e2ee_keys") ?: "{\"keys\":{},\"active\":{}}")
    private fun slot(aud:String,cid:Long)=b64(aud.toByteArray(Charsets.UTF_8))+":"+cid
    private fun key(cid:Long,kid:String?=null):JSONObject {
        val all=keys();val slot=slot(audience(),cid)
        val id=kid ?: all.getJSONObject("active").optString(slot)
        val code=all.getJSONObject("keys").optJSONObject("$slot:$id") ?: throw IllegalStateException("Нет ключа разговора. Откройте «Шифрование» и подключите код с доверенного устройства")
        require(code.getString("aud")==audience() && code.getLong("cid")==cid && digest(unb64(code.getString("key")))==id) {"Повреждено локальное хранилище ключей"}
        return code
    }
    @Synchronized fun fingerprint(cid:Long):String = digest(unb64(key(cid).getString("key")))
    @Synchronized fun exportKey(cid:Long):String = "poknite-key-v1:"+b64(key(cid).toString().toByteArray(Charsets.UTF_8))
    @Synchronized fun createKey(cid:Long,members:JSONArray):String {
        val secret=ByteArray(32).also {random.nextBytes(it)}
        val code=JSONObject().put("v",1).put("aud",audience()).put("cid",cid).put("members",membersDigest(members)).put("key",b64(secret))
        secret.fill(0);save(code);store.unlockHistory(this,cid)
        return "poknite-key-v1:"+b64(code.toString().toByteArray(Charsets.UTF_8))
    }
    @Synchronized fun importKey(cid:Long,members:JSONArray,token:String) {
        require(token.length<=2048) {"Код ключа слишком длинный"}
        val code=try {require(token.trim().startsWith("poknite-key-v1:"));JSONObject(String(unb64(token.trim().removePrefix("poknite-key-v1:")),Charsets.UTF_8))} catch(_:Exception){throw IllegalArgumentException("Неверный код ключа разговора")}
        require(code.getInt("v")==1 && unb64(code.getString("key")).size==32) {"Неверный код ключа разговора"}
        require(code.getString("aud")==audience() && code.getLong("cid")==cid) {"Ключ предназначен для другого сервера или разговора"}
        require(code.getString("members")==membersDigest(members)) {"Состав устройств изменился. Создайте новый ключ на доверенном устройстве"}
        save(code);store.unlockHistory(this,cid)
    }
    private fun save(code:JSONObject) {
        val all=keys();val entries=all.getJSONObject("keys");val slot=slot(code.getString("aud"),code.getLong("cid"));val id=digest(unb64(code.getString("key")))
        require(entries.has("$slot:$id") || entries.length()<1024) {"Локальное хранилище ключей заполнено"}
        val changed=all.getJSONObject("active").optString(slot)!=id
        entries.put("$slot:$id",code);all.getJSONObject("active").put(slot,id)
        settings.saveSecret("e2ee_keys",all.toString())
        if(changed)store.rotateDraftKey(code.getString("aud"),code.getLong("cid"))
    }
    @Synchronized fun seal(draft:Draft,members:JSONArray):JSONObject {
        require(validText(draft.text));UUID.fromString(draft.clientId)
        val aud=audience();val mentions=JSONArray(draft.mentions)
        val original=JSONObject().put("client_message_id",draft.clientId).put("text",draft.text).put("mentions",mentions)
        val hash=digest(original.toString().toByteArray(Charsets.UTF_8))
        val cached=store.sealedDraft(aud,draft.channelId)
        if(cached!=null && cached[0]==draft.clientId) {
            require(cached[1]==hash) {"Идентификатор черновика уже использован"}
            return JSONObject().put("client_message_id",draft.clientId).put("text",cached[2]).put("mentions",mentions)
        }
        val code=key(draft.channelId)
        require(code.getString("members")==membersDigest(members)) {"Состав устройств изменился. Смените ключ в разделе «Шифрование»"}
        require((0 until members.length()).any {val m=members.getJSONObject(it);m.getLong("user_id")==settings.userId && m.getLong("device_id")==settings.deviceId}) {"Ваше устройство отсутствует в разговоре"}
        val secret=unb64(code.getString("key"))
        val header=JSONObject().put("alg","dir").put("enc","A256GCM").put("v",1).put("kid",digest(secret)).put("aud",aud).put("cid",draft.channelId).put("sid",settings.userId).put("did",settings.deviceId).put("mid",draft.clientId).put("members",code.getString("members")).put("mentions",mentions)
        val nonce=ByteArray(12).also {random.nextBytes(it)}
        val sealed=encrypt(secret,header,draft.text,nonce);secret.fill(0)
        store.saveSealedDraft(aud,draft.channelId,draft.clientId,hash,sealed)
        return JSONObject().put("client_message_id",draft.clientId).put("text",sealed).put("mentions",mentions)
    }
    @Synchronized fun decodeWire(wire:JSONObject):WireMessage {
        val original=wire.toString()
        var parsed:Parsed?=null
        val placeholder=JSONObject(original).put("text","🔒 Сообщение недоступно: нужен ключ разговора или данные повреждены").put("mentions",JSONArray())
        try {
            parsed=parse(wire.getString("text"));val h=parsed.header
            require(h.getString("aud")==audience() && h.getLong("cid")==wire.getLong("channel_id") && h.getLong("sid")==wire.getLong("sender_id"))
            require(sameMentions(h.getJSONArray("mentions"),wire.optJSONArray("mentions") ?: JSONArray()))
            val code=key(h.getLong("cid"),h.getString("kid"));require(code.getString("members")==h.getString("members"))
            val secret=unb64(code.getString("key"));val text=decrypt(secret,parsed);secret.fill(0)
            require(validText(text));val chars=text.codePoints().toArray();val mentions=h.getJSONArray("mentions")
            for(i in 0 until mentions.length()){val m=mentions.getJSONObject(i);require(m.getInt("end")<=chars.size && chars[m.getInt("start")]=='@'.code)}
            val plain=JSONObject(original).put("id","${h.getLong("did")}:${h.getString("mid")}").put("text",text)
            return WireMessage.parse(plain)
        } catch(_:Exception) {
            parsed?.let {placeholder.put("id","${it.header.getLong("did")}:${it.header.getString("mid")}")}
            return WireMessage.parse(placeholder).copy(sealed=if(parsed!=null)original else null,verified=false)
        }
    }
    companion object {
        private const val FLAGS=Base64.URL_SAFE or Base64.NO_PADDING or Base64.NO_WRAP
        fun b64(bytes:ByteArray):String=Base64.encodeToString(bytes,FLAGS)
        fun unb64(value:String):ByteArray=Base64.decode(value,FLAGS).also {require(b64(it)==value)}
        fun digest(bytes:ByteArray):String=b64(MessageDigest.getInstance("SHA-256").digest(bytes))
        fun membersDigest(members:JSONArray):String {
            require(members.length() in 1..1000)
            val pairs=(0 until members.length()).map {val m=members.getJSONObject(it);m.getLong("user_id") to m.getLong("device_id")}.sortedWith(compareBy<Pair<Long,Long>> {it.first}.thenBy {it.second})
            require(pairs.all {it.first>0 && it.second>0} && pairs.map {it.second}.distinct().size==pairs.size)
            val prefix="Poknite members v1\u0000".toByteArray(Charsets.UTF_8)
            val bytes=ByteBuffer.allocate(prefix.size+pairs.size*16).put(prefix)
            pairs.forEach {bytes.putLong(it.first).putLong(it.second)}
            return digest(bytes.array())
        }
        data class Parsed(val header:JSONObject,val protected:String,val nonce:ByteArray,val ciphertext:ByteArray,val tag:ByteArray)
        fun parse(text:String):Parsed {
            require(text.length<=12288 && text.startsWith("e2ee:"))
            val f=text.removePrefix("e2ee:").split('.');require(f.size==5 && f[1].isEmpty() && f[0].length<=8192)
            val h=JSONObject(String(unb64(f[0]),Charsets.UTF_8))
            require(h.length()==11 && h.getString("alg")=="dir" && h.getString("enc")=="A256GCM" && h.getInt("v")==1)
            require(unb64(h.getString("kid")).size==32 && unb64(h.getString("members")).size==32)
            require(h.getString("aud").length in 1..512 && h.getLong("cid")>0 && h.getLong("sid")>0 && h.getLong("did")>0)
            require(h.getString("mid").length==36);UUID.fromString(h.getString("mid"))
            val mentions=h.getJSONArray("mentions");require(mentions.length()<=32);var end=0
            for(i in 0 until mentions.length()){val m=mentions.getJSONObject(i);require(m.getLong("user_id")>0 && m.getInt("start")>=end && m.getInt("start")<m.getInt("end") && m.getInt("end")<=4096);end=m.getInt("end")}
            return Parsed(h,f[0],unb64(f[2]),unb64(f[3]),unb64(f[4])).also {require(it.nonce.size==12 && it.ciphertext.size in 1..4096 && it.tag.size==16)}
        }
        fun encrypt(secret:ByteArray,header:JSONObject,text:String,nonce:ByteArray):String {
            require(secret.size==32 && nonce.size==12 && validText(text))
            val protected=b64(header.toString().toByteArray(Charsets.UTF_8))
            val cipher=Cipher.getInstance("AES/GCM/NoPadding");cipher.init(Cipher.ENCRYPT_MODE,SecretKeySpec(secret,"AES"),GCMParameterSpec(128,nonce));cipher.updateAAD(protected.toByteArray(Charsets.US_ASCII))
            val encrypted=cipher.doFinal(text.toByteArray(Charsets.UTF_8));val tag=encrypted.copyOfRange(encrypted.size-16,encrypted.size)
            return "e2ee:$protected..${b64(nonce)}.${b64(encrypted.copyOfRange(0,encrypted.size-16))}.${b64(tag)}".also {parse(it)}
        }
        fun decrypt(secret:ByteArray,sealed:Parsed):String {
            val cipher=Cipher.getInstance("AES/GCM/NoPadding");cipher.init(Cipher.DECRYPT_MODE,SecretKeySpec(secret,"AES"),GCMParameterSpec(128,sealed.nonce));cipher.updateAAD(sealed.protected.toByteArray(Charsets.US_ASCII))
            val plain=cipher.doFinal(sealed.ciphertext+sealed.tag)
            return Charsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT).onUnmappableCharacter(CodingErrorAction.REPORT).decode(ByteBuffer.wrap(plain)).toString()
        }
        private fun sameMentions(a:JSONArray,b:JSONArray):Boolean = a.length()==b.length() && (0 until a.length()).all {val x=a.getJSONObject(it);val y=b.getJSONObject(it);x.getLong("user_id")==y.getLong("user_id") && x.getInt("start")==y.getInt("start") && x.getInt("end")==y.getInt("end")}
    }
}
