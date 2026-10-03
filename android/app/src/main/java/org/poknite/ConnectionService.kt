package org.poknite

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.net.ConnectivityManager
import android.net.Network
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.json.JSONObject
import java.util.concurrent.atomic.AtomicLong

class ConnectionService : Service() {
    private val app get() = application as PokniteApp
    private val main = Handler(Looper.getMainLooper())
    private val generation = AtomicLong(0)
    @Volatile private var socket: WebSocket? = null
    @Volatile private var stopping = false
    private var retry = 0
    private var networkRegistered = false
    private val manager get() = getSystemService(NotificationManager::class.java)
    private val network = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) { main.post { if (!stopping) reconnectNow() } }
        override fun onLost(network: Network) { main.post { if (!stopping) reconnectNow() } }
    }
    override fun onCreate() {
        super.onCreate()
        manager.createNotificationChannel(NotificationChannel("connection", "Подключение Poknite", NotificationManager.IMPORTANCE_LOW).apply { setShowBadge(false) })
        val n = serviceNotification("Подключение…")
        if (Build.VERSION.SDK_INT >= 34) startForeground(1, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE) else startForeground(1, n)
    }
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == STOP) { app.settings.enabled = false; shutdown("Отключено"); return START_NOT_STICKY }
        if (!app.settings.enabled || app.settings.token.isEmpty()) { shutdown("Подключите устройство по приглашению"); return START_NOT_STICKY }
        if (!networkRegistered) {
            getSystemService(ConnectivityManager::class.java).registerDefaultNetworkCallback(network)
            networkRegistered = true
            reconnectNow()
        }
        status(app.settings.status)
        return START_STICKY
    }
    private fun status(value: String) {
        app.settings.status = value
        main.post { if (!stopping) manager.notify(1, serviceNotification(value)); app.changed() }
    }
    private fun serviceNotification(text: String): Notification {
        val open = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
        val stop = PendingIntent.getService(this, 1, Intent(this, ConnectionService::class.java).setAction(STOP), PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
        return Notification.Builder(this, "connection").setSmallIcon(R.drawable.ic_notification).setContentTitle("Poknite").setContentText(text).setContentIntent(open).setOngoing(true).setOnlyAlertOnce(true).addAction(Notification.Action.Builder(null, "Отключить", stop).build()).build()
    }
    private fun reconnectNow() {
        if (stopping || !app.settings.enabled) return
        main.removeCallbacksAndMessages(null)
        generation.incrementAndGet()
        socket?.cancel(); socket = null
        app.online = false
        connect()
    }
    private fun connect() {
        if (stopping || !app.settings.enabled) return
        val g = generation.incrementAndGet()
        status("Подключение…")
        app.io.execute {
            if (stopping || g != generation.get()) return@execute
            try {
                val endpoint = canonicalEndpoint(app.settings.endpoint, BuildConfig.DEBUG)
                val wsUrl = (if (endpoint.startsWith("https:")) endpoint.replaceFirst("https:", "wss:") else endpoint.replaceFirst("http:", "ws:")) + "/v2/stream?after=${app.store.cursor()}"
                val request = Request.Builder().url(wsUrl).header("Authorization", "Bearer ${app.settings.token}").build()
                var stateParts:JSONObject?=null
                val newSocket = app.http.newWebSocket(request, object : WebSocketListener() {
                    override fun onMessage(webSocket: WebSocket, text: String) {
                        if (g != generation.get() || stopping) return
                        if (text.toByteArray().size > 32768) { webSocket.cancel(); failed(g, "Слишком большое сообщение сервера"); return }
                        // Process on the socket reader: disk backpressure bounds incoming work.
                        synchronized(app.store) {
                            if (g != generation.get() || stopping) return
                            try {
                                val j = JSONObject(text)
                                when (j.getString("type")) {
                                    "hello" -> {
                                        require(j.getLong("user_id") == app.settings.userId && j.getLong("device_id") == app.settings.deviceId)
                                        if(j.getJSONArray("channels").length()>0)updateChannels(j.getJSONArray("channels")); status("Восстановление истории…")
                                    }
                                    "channels" -> { updateChannels(j.getJSONArray("channels")); app.changed() }
                                    "profiles" -> { app.store.updateUsers(j.getJSONArray("users"));app.changed() }
                                    "state_part" -> {
                                        if(j.getBoolean("first")) stateParts=JSONObject().put("profile",j.getJSONObject("profile")).put("contacts",org.json.JSONArray()).put("channels",org.json.JSONArray())
                                        val parts=stateParts ?: error("Неполное состояние")
                                        for(key in listOf("contacts","channels")){val rows=j.getJSONArray(key);val all=parts.getJSONArray(key);for(i in 0 until rows.length())all.put(rows.get(i))}
                                        if(j.getBoolean("last")){applyState(parts);stateParts=null}
                                    }
                                    "state" -> {
                                        applyState(j)
                                    }
                                    "message" -> {
                                        val replay = j.getBoolean("replay")
                                        app.store.receive(app.e2ee.decodeWire(j.getJSONObject("message")), replay, app.settings.userId, notify = j.getBoolean("notify"))
                                        if (!replay) { deliver(false); app.changed() }
                                    }
                                    "progress" -> { app.store.progress(j.getLong("cursor")); acknowledge(webSocket, app.store.cursor()) }
                                    "reset" -> { app.store.progress(j.getLong("cursor"), true); acknowledge(webSocket, app.store.cursor()) }
                                    "synced" -> {
                                        app.store.progress(j.getLong("cursor")); acknowledge(webSocket, app.store.cursor())
                                        deliver(true); deliver(false); retry = 0; app.online = true
                                        status(if (j.optBoolean("gap")) "Подключено · часть сообщений уже истекла" else "Подключено")
                                    }
                                    else -> error("Неизвестная версия протокола")
                                }
                            } catch (_: Exception) { webSocket.cancel(); failed(g, "Ошибка протокола или локального хранилища") }
                        }
                    }
                    override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                        if (code == 4001) revoked(g) else failed(g, "Соединение закрыто")
                    }
                    override fun onClosing(webSocket: WebSocket, code: Int, reason: String) { webSocket.close(code, null); if (code == 4001) revoked(g) }
                    override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                        if (response?.code == 401) revoked(g) else failed(g, "Нет соединения · повторяем подключение")
                    }
                })
                main.post { if (g == generation.get() && !stopping) socket = newSocket else newSocket.cancel() }
            } catch (_: Exception) { failed(g, "Не удалось подключиться") }
        }
    }
    private fun applyState(j:JSONObject) {
        val profile=j.getJSONObject("profile");app.settings.profile=profile.toString()
        val contacts=j.getJSONArray("contacts");app.settings.contacts=contacts.toString()
        app.store.updateUsers(contacts);app.store.updateUsers(org.json.JSONArray().put(profile));updateChannels(j.getJSONArray("channels"));app.changed()
    }
    private fun updateChannels(channels: org.json.JSONArray) {
        val cancelled=app.store.updateChannels(channels)
        cancelled.forEach { manager.cancel(it,2) }
        if(cancelled.isNotEmpty()) manager.cancel("summary",2)
    }
    private fun acknowledge(ws: WebSocket, cursor: Long) { check(ws.send(JSONObject().put("type", "ack").put("cursor", cursor).toString())) }
    private fun failed(g: Long, reason: String) {
        main.post {
            if (g != generation.get() || stopping) return@post
            generation.incrementAndGet(); socket?.cancel(); socket = null; app.online = false
            status(reason)
            main.postDelayed({ connect() }, retryDelay(retry++))
        }
    }
    private fun revoked(g: Long) { main.post { if (g == generation.get()) { app.settings.enabled = false; updateChannels(org.json.JSONArray()); shutdown("Устройство отозвано · нужно новое приглашение") } } }
    private fun deliver(replay: Boolean) {
        val pending = app.store.pending(replay)
        if (pending.isEmpty() || !manager.areNotificationsEnabled() || Build.VERSION.SDK_INT >= 33 && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) return
        val quiet = pending.all { app.store.quiet(it.channelId) }
        val channelId = if (quiet) "messages_quiet" else "messages"
        manager.createNotificationChannel(NotificationChannel(channelId, if (quiet) "Сообщения без звука" else "Сообщения", if (quiet) NotificationManager.IMPORTANCE_LOW else NotificationManager.IMPORTANCE_DEFAULT))
        val open = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
        val last = pending.last()
        val summary = replay || pending.size > 1
        if (!summary) {
            val active = manager.activeNotifications.filter { it.tag != null }.sortedBy { it.postTime }
            if (active.size >= 45) active.take(active.size - 44).forEach { manager.cancel(it.tag, it.id) }
        }
        val title = if (summary) "Poknite · ${pending.size} новых сообщений" else last.senderName
        val text = if (summary) "Откройте локальную историю" else last.text
        // Fixed summary ID and deterministic message tags make recovery idempotent.
        manager.notify(if (summary) "summary" else last.id, 2, Notification.Builder(this, channelId).setSmallIcon(R.drawable.ic_notification).setContentTitle(title).setContentText(text).setStyle(Notification.BigTextStyle().bigText(text)).setContentIntent(open).setAutoCancel(true).setOnlyAlertOnce(true).setGroup("poknite.messages").setGroupSummary(summary).build())
        app.store.shown(pending.map { it.id })
    }
    private fun shutdown(reason: String) {
        app.settings.status = reason; app.changed(); stopping = true; app.online = false
        generation.incrementAndGet(); socket?.cancel(); socket = null
        main.removeCallbacksAndMessages(null)
        stopForeground(STOP_FOREGROUND_REMOVE); stopSelf()
    }
    override fun onDestroy() {
        stopping = true; app.online = false; generation.incrementAndGet(); socket?.cancel(); main.removeCallbacksAndMessages(null)
        if (networkRegistered) runCatching { getSystemService(ConnectivityManager::class.java).unregisterNetworkCallback(network) }
        super.onDestroy()
    }
    override fun onBind(intent: Intent?): IBinder? = null
    companion object { const val STOP = "org.poknite.STOP" }
}
