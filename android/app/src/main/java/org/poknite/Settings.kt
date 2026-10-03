package org.poknite

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

class Settings(context: Context, name: String = "settings") {
    private val p = context.getSharedPreferences(name, Context.MODE_PRIVATE)
    @Synchronized fun secret(name: String): String? {
        val value = p.getString(name, null) ?: return null
        try {
            val bytes = Base64.decode(value, Base64.NO_WRAP)
            require(bytes.size > 28)
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, bytes.copyOfRange(0,12)))
            cipher.updateAAD(name.toByteArray(Charsets.UTF_8))
            return cipher.doFinal(bytes.copyOfRange(12,bytes.size)).toString(Charsets.UTF_8)
        } catch (_: Exception) { throw IllegalStateException("Не удалось открыть локальное хранилище ключей") }
    }
    @Synchronized fun saveSecret(name: String, value: String) {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, key())
        cipher.updateAAD(name.toByteArray(Charsets.UTF_8))
        val bytes = cipher.iv + cipher.doFinal(value.toByteArray(Charsets.UTF_8))
        check(p.edit().putString(name, Base64.encodeToString(bytes, Base64.NO_WRAP)).commit())
    }
    var selectedConversation: Long get() = p.getLong("selected_conversation", 0); set(v) { require(v >= 0); check(p.edit().putLong("selected_conversation", v).commit()) }
    var endpoint: String get() = p.getString("endpoint", "")!!; set(v) { check(p.edit().putString("endpoint", v).commit()) }
    var managementEndpoint: String get() = p.getString("management_endpoint", "")!!; set(v) { check(p.edit().putString("management_endpoint", v).commit()) }
    var managementIp: String get() = p.getString("management_ip", "")!!; set(v) { check(p.edit().putString("management_ip", v).commit()) }
    var profile: String get() = p.getString("profile", "{}")!!; set(v) { check(p.edit().putString("profile", v).commit()) }
    var contacts: String get() = p.getString("contacts", "[]")!!; set(v) { check(p.edit().putString("contacts", v).commit()) }
    var userId: Long get() = p.getLong("user_id", 0); set(v) { check(p.edit().putLong("user_id", v).commit()) }
    var deviceId: Long get() = p.getLong("device_id", 0); set(v) { check(p.edit().putLong("device_id", v).commit()) }
    var enabled: Boolean get() = p.getBoolean("enabled", false); set(v) { check(p.edit().putBoolean("enabled", v).commit()) }
    var status: String get() = p.getString("status", "Отключено")!!; set(v) { p.edit().putString("status", v).apply() }
    var token: String
        get() {
            val value = p.getString("token", null) ?: return ""
            return try {
                val b = Base64.decode(value, Base64.NO_WRAP)
                require(b.size > 28)
                val c = Cipher.getInstance("AES/GCM/NoPadding")
                c.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, b.copyOfRange(0, 12)))
                c.doFinal(b.copyOfRange(12, b.size)).toString(Charsets.UTF_8)
            } catch (_: Exception) { "" }
        }
        set(value) {
            if (value.isEmpty()) { check(p.edit().remove("token").commit()); return }
            require(value.matches(Regex("[0-9a-f]{64}")))
            val c = Cipher.getInstance("AES/GCM/NoPadding")
            c.init(Cipher.ENCRYPT_MODE, key())
            val encrypted = Base64.encodeToString(c.iv + c.doFinal(value.toByteArray()), Base64.NO_WRAP)
            check(p.edit().putString("token", encrypted).commit())
        }
    private fun key(): SecretKey {
        val k = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (k.getKey("poknite-token", null) as? SecretKey)?.let { return it }
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").apply {
            init(KeyGenParameterSpec.Builder("poknite-token", KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT).setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).build())
        }.generateKey()
    }
}
