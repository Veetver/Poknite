package org.poknite

import android.Manifest
import android.app.Activity
import android.app.AlertDialog
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.provider.Settings as SystemSettings
import android.text.Editable
import android.text.InputFilter
import android.text.TextWatcher
import android.view.View
import android.widget.*
import org.json.JSONArray
import org.json.JSONObject
import java.text.DateFormat
import java.util.Date

class MainActivity : Activity() {
    private val app get() = application as PokniteApp
    private val handler = Handler(Looper.getMainLooper())
    private lateinit var root: LinearLayout
    private lateinit var status: TextView
    private lateinit var battery: TextView
    private var composer: EditText? = null
    private var picker: Spinner? = null
    private var history: TextView? = null
    private var quiet: Switch? = null
    private var channels = emptyList<Channel>()
    private var selected = 0L
    private var composerChannel = 0L
    private var page = 0
    private var binding = false
    private var busy = false
    private var registered = false
    private val save = Runnable { saveComposer() }
    private val changed = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) { refresh() }
    }
    private fun vertical(): LinearLayout = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setPadding(dp(16), dp(8), dp(16), dp(8)) }
    private fun dp(value: Int) = (value * resources.displayMetrics.density).toInt()
    private fun text(value: String, size: Float = 16f): TextView = TextView(this).apply { text = value; textSize = size; setPadding(0, dp(6), 0, dp(6)) }
    private fun button(label: String, action: () -> Unit): Button = Button(this).apply { text = label; setOnClickListener { action() } }
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        selected = savedInstanceState?.getLong("selected") ?: 0
        render()
    }
    override fun onSaveInstanceState(out: Bundle) { out.putLong("selected", selected); super.onSaveInstanceState(out) }
    private fun render() {
        handler.removeCallbacks(save)
        composer = null; composerChannel = 0; picker = null; history = null; quiet = null
        val scroll = ScrollView(this)
        root = vertical()
        scroll.addView(root)
        scroll.setOnApplyWindowInsetsListener { v, insets ->
            if (Build.VERSION.SDK_INT >= 30) {
                val padding = insets.getInsets(android.view.WindowInsets.Type.systemBars() or android.view.WindowInsets.Type.ime())
                v.setPadding(padding.left, padding.top, padding.right, padding.bottom)
            } else v.setPadding(insets.systemWindowInsetLeft, insets.systemWindowInsetTop, insets.systemWindowInsetRight, insets.systemWindowInsetBottom)
            insets
        }
        setContentView(scroll)
        root.addView(text("Poknite", 26f))
        status = text(app.settings.status)
        root.addView(status)
        battery = text("")
        root.addView(battery)
        if (app.settings.token.isEmpty()) setup() else connected()
        updateStatus()
    }
    private fun setup() {
        root.addView(text("Подключение к своему серверу", 20f))
        val endpoint = EditText(this).apply { hint = "https://notify.example.org"; setText(app.settings.endpoint); inputType = android.text.InputType.TYPE_CLASS_TEXT or android.text.InputType.TYPE_TEXT_VARIATION_URI; setSingleLine(true) }
        val invitation = EditText(this).apply { hint = "Одноразовое приглашение"; inputType = android.text.InputType.TYPE_CLASS_TEXT or android.text.InputType.TYPE_TEXT_VARIATION_PASSWORD; setSingleLine(true) }
        val name = EditText(this).apply { hint = "Имя устройства"; setText(Build.MODEL.take(64)); setSingleLine(true); filters = arrayOf(InputFilter.LengthFilter(64)) }
        root.addView(endpoint); root.addView(invitation); root.addView(name)
        val enroll = button("Подключить") {
            if (busy) return@button
            val address = runCatching { canonicalEndpoint(endpoint.text.toString(), BuildConfig.DEBUG) }.getOrElse { showError(it.message ?: "Проверьте адрес"); return@button }
            val code = invitation.text.toString().trim()
            val deviceName = name.text.toString().trim()
            if (!code.matches(Regex("[0-9a-f]{64}")) || deviceName.isBlank() || deviceName.any { it.isISOControl() }) { showError("Проверьте приглашение и имя устройства"); return@button }
            work({ JSONObject(app.request("/v1/devices/enroll", "POST", JSONObject().put("invitation", code).put("device_name", deviceName), address, false)) }) { result ->
                app.settings.enabled = false; stopService(Intent(this, ConnectionService::class.java))
                app.store.resetAccount()
                app.settings.endpoint = address; app.settings.userId = result.getLong("user_id"); app.settings.deviceId = result.getLong("device_id"); app.settings.token = result.getString("token")
                app.settings.status = "Устройство подключено"
                selected = 0; render(); enable()
            }
        }
        root.addView(enroll)
        root.addView(text("Приглашение выдаёт администратор. Оно действует 15 минут и используется один раз."))
        if (BuildConfig.DEBUG) root.addView(text("Тестовая сборка: HTTP разрешён только для localhost и адреса эмулятора 10.0.2.2."))
    }
    private fun connected() {
        root.addView(text(app.settings.endpoint, 13f))
        val row = LinearLayout(this)
        row.addView(button("Приём") { if (app.settings.enabled) disable() else enable() }, LinearLayout.LayoutParams(0, -2, 1f))
        row.addView(button("Настройки") { options() }, LinearLayout.LayoutParams(0, -2, 1f))
        root.addView(row)
        channels = app.store.channels()
        picker = Spinner(this).also { s ->
            root.addView(s)
            s.onItemSelectedListener = object : AdapterView.OnItemSelectedListener {
                override fun onNothingSelected(parent: AdapterView<*>?) = Unit
                override fun onItemSelected(parent: AdapterView<*>?, view: View?, position: Int, id: Long) {
                    if (binding || position !in channels.indices) return
                    saveComposer()
                    selected = channels[position].id; page = 0; loadChannel()
                }
            }
        }
        quiet = Switch(this).apply {
            text = "Без звука в этом канале"
            setOnCheckedChangeListener { _, value -> if (!binding && selected > 0) app.store.setQuiet(selected, value) }
        }.also { root.addView(it) }
        composer = EditText(this).apply {
            hint = "Сообщение · до 4096 байт"
            minLines = 2; maxLines = 6
            inputType = android.text.InputType.TYPE_CLASS_TEXT or android.text.InputType.TYPE_TEXT_FLAG_MULTI_LINE or android.text.InputType.TYPE_TEXT_FLAG_CAP_SENTENCES
            filters = arrayOf(InputFilter { source, start, end, dest, dstart, dend ->
                val candidate = dest.subSequence(0, dstart).toString() + source.subSequence(start, end) + dest.subSequence(dend, dest.length)
                if (candidate.toByteArray().size > 4096 || candidate.any { it.isISOControl() && it !in "\n\r\t" }) "" else null
            })
            addTextChangedListener(object : TextWatcher {
                override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) = Unit
                override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) { if (!binding) { handler.removeCallbacks(save); handler.postDelayed(save, 400) } }
                override fun afterTextChanged(s: Editable?) = Unit
            })
        }.also { root.addView(it) }
        root.addView(button("Отправить") { publish() })
        root.addView(text("Черновик сохраняется. При отсутствии связи отправка выполняется вручную после подключения.", 13f))
        root.addView(text("Локальная история", 20f))
        history = text("").also { it.setTextIsSelectable(true); root.addView(it) }
        val pages = LinearLayout(this)
        pages.addView(button("Новее") { page = (page - 1).coerceAtLeast(0); loadHistory() }, LinearLayout.LayoutParams(0, -2, 1f))
        pages.addView(button("Старее") { if (app.store.history(selected, (page + 1) * 50).isNotEmpty()) { page++; loadHistory() } }, LinearLayout.LayoutParams(0, -2, 1f))
        root.addView(pages)
        refreshChannels()
    }
    private fun refreshChannels() {
        val current = app.store.channels()
        if (current == channels && picker?.adapter != null) return
        saveComposer()
        channels = current
        binding = true
        picker?.adapter = ArrayAdapter(this, android.R.layout.simple_spinner_dropdown_item, channels.map { it.name })
        if (channels.none { it.id == selected }) selected = channels.firstOrNull()?.id ?: 0
        picker?.setSelection(channels.indexOfFirst { it.id == selected }.coerceAtLeast(0))
        binding = false
        loadChannel()
    }
    private fun loadChannel() {
        handler.removeCallbacks(save)
        binding = true
        composer?.setText(app.store.draft(selected)?.text ?: "")
        composerChannel = selected
        composer?.isEnabled = selected > 0
        quiet?.isChecked = app.store.quiet(selected)
        binding = false
        loadHistory()
    }
    private fun loadHistory() {
        if (selected == 0L) { history?.text = "Ожидаем список каналов от сервера…"; return }
        val format = DateFormat.getDateTimeInstance(DateFormat.SHORT, DateFormat.SHORT)
        history?.text = app.store.history(selected, page * 50).joinToString("\n\n") { "${it.senderName} · ${format.format(Date(it.createdAt * 1000))}\n${it.text}" }.ifEmpty { "В этом канале пока нет сообщений" }
    }
    private fun saveComposer() { if (!binding && selected > 0 && composerChannel == selected) composer?.let { app.store.saveDraft(selected, it.text.toString()) } }
    private fun publish() {
        if (busy || selected == 0L) return
        val value = composer?.text.toString()
        if (!validText(value)) { showError("Нужен непустой текст до 4096 байт UTF-8"); return }
        saveComposer()
        if (!app.online) { showError("Сейчас нет связи. Черновик сохранён; отправьте после подключения."); return }
        val draft = app.store.draft(selected) ?: return
        work({
            val m = WireMessage.parse(JSONObject(app.request("/v1/channels/${draft.channelId}/messages", "POST", JSONObject().put("client_message_id", draft.clientId).put("text", draft.text))))
            app.store.receive(m, false, app.settings.userId, false)
            app.store.sentDraft(draft)
        }) {
            if (selected == draft.channelId && composer?.text.toString() == draft.text) { binding = true; composer?.setText(""); binding = false }
            loadHistory()
        }
    }
    private fun enable() {
        if (Build.VERSION.SDK_INT >= 33 && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) { requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 10); return }
        app.settings.enabled = true
        runCatching { startForegroundService(Intent(this, ConnectionService::class.java)) }.onFailure { app.settings.enabled = false; showError("Не удалось включить фоновый приём. Откройте приложение повторно.") }
        updateStatus()
    }
    private fun disable() { app.settings.enabled = false; app.settings.status = "Отключено"; stopService(Intent(this, ConnectionService::class.java)); app.online = false; updateStatus() }
    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == 10 && grantResults.firstOrNull() == PackageManager.PERMISSION_GRANTED) enable()
        else if (requestCode == 10) showError("Разрешите уведомления в настройках Android, чтобы включить приём.")
    }
    private fun options() {
        if (busy) { showError("Дождитесь завершения текущей операции"); return }
        AlertDialog.Builder(this).setTitle("Настройки").setItems(arrayOf("Мои устройства", "Фоновая работа и батарея", "Разрешения уведомлений", "Очистить локальную историю", "Новое подключение")) { _, i ->
            when (i) {
                0 -> devices()
                1 -> batterySetup()
                2 -> startActivity(Intent(SystemSettings.ACTION_APP_NOTIFICATION_SETTINGS).putExtra(SystemSettings.EXTRA_APP_PACKAGE, packageName))
                3 -> AlertDialog.Builder(this).setMessage("Удалить полученные сообщения на этом устройстве?").setNegativeButton("Отмена", null).setPositiveButton("Очистить") { _, _ -> app.store.clearHistory(); val nm = getSystemService(android.app.NotificationManager::class.java); nm.activeNotifications.filter { it.tag != null }.forEach { nm.cancel(it.tag, it.id) }; page = 0; loadHistory() }.show()
                4 -> AlertDialog.Builder(this).setMessage("Сбросить подключение и локальную историю? Для подключения потребуется новое приглашение. Старое устройство можно отозвать через другое своё устройство.").setNegativeButton("Отмена", null).setPositiveButton("Сбросить") { _, _ -> disable(); app.settings.token = ""; app.store.resetAccount(); selected = 0; render() }.show()
            }
        }.show()
    }
    private fun devices() {
        work({ JSONArray(app.request("/v1/devices")) }) { devices ->
            val names = Array(devices.length()) { i -> devices.getJSONObject(i).let { it.getString("name") + if (it.getBoolean("current")) " (это устройство)" else "" } }
            AlertDialog.Builder(this).setTitle("Выберите устройство для отзыва").setItems(names) { _, i ->
                val d = devices.getJSONObject(i)
                AlertDialog.Builder(this).setMessage("Отозвать «${d.getString("name") }»?").setNegativeButton("Отмена", null).setPositiveButton("Отозвать") { _, _ ->
                    work({ app.request("/v1/devices/${d.getLong("id")}", "DELETE") }) { if (d.getBoolean("current")) { disable(); app.settings.token = ""; app.settings.status = "Устройство отозвано"; app.store.resetAccount(); selected = 0; render() } else Toast.makeText(this, "Устройство отозвано", Toast.LENGTH_SHORT).show() }
                }.show()
            }.setNegativeButton("Закрыть", null).show()
        }
    }
    // Direct instant messaging without an external push provider is the documented exemption use case.
    @android.annotation.SuppressLint("BatteryLife")
    private fun batterySetup() {
        val pm = getSystemService(PowerManager::class.java)
        AlertDialog.Builder(this).setTitle("Фоновая доставка").setMessage("Постоянное уведомление показывает состояние приёма. Исключение из оптимизации батареи помогает доставке при выключенном экране. Некоторые телефоны дополнительно требуют разрешения автозапуска. После принудительной остановки откройте Poknite снова.").setNegativeButton("Закрыть", null).setPositiveButton(if (pm.isIgnoringBatteryOptimizations(packageName)) "Настройки батареи" else "Разрешить работу") { _, _ ->
            runCatching { startActivity(if (pm.isIgnoringBatteryOptimizations(packageName)) Intent(SystemSettings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS) else Intent(SystemSettings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:$packageName"))) }.onFailure { showError("Откройте настройки батареи Android вручную") }
        }.show()
    }
    private fun updateStatus() {
        status.text = app.settings.status + if (!app.settings.enabled) " · приём выключен" else ""
        battery.text = if (getSystemService(PowerManager::class.java).isIgnoringBatteryOptimizations(packageName)) "Батарея: фоновая работа разрешена" else "Батарея: действует оптимизация Android"
    }
    private fun refresh() { if (isFinishing || isDestroyed) return; updateStatus(); refreshChannels(); loadHistory() }
    // API 26–32 use our signature permission; NOT_EXPORTED is available starting at 33.
    @android.annotation.SuppressLint("UnspecifiedRegisterReceiverFlag")
    override fun onStart() {
        super.onStart()
        if (Build.VERSION.SDK_INT >= 33) registerReceiver(changed, IntentFilter("org.poknite.CHANGED"), RECEIVER_NOT_EXPORTED) else registerReceiver(changed, IntentFilter("org.poknite.CHANGED"), "org.poknite.INTERNAL", null)
        registered = true; refresh()
        if (app.settings.enabled && app.settings.token.isNotEmpty()) runCatching { startForegroundService(Intent(this, ConnectionService::class.java)) }
    }
    override fun onStop() { handler.removeCallbacks(save); saveComposer(); if (registered) { unregisterReceiver(changed); registered = false }; super.onStop() }
    override fun onDestroy() { handler.removeCallbacksAndMessages(null); super.onDestroy() }
    private fun showError(message: String) { if (!isFinishing && !isDestroyed) AlertDialog.Builder(this).setTitle("Poknite").setMessage(message).setPositiveButton("Понятно", null).show() }
    private fun <T> work(job: () -> T, done: (T) -> Unit) {
        if (busy) return
        busy = true
        app.io.execute {
            val result = runCatching(job)
            runOnUiThread {
                busy = false
                if (!isFinishing && !isDestroyed) result.fold({ value -> runCatching { done(value) }.onFailure { showError("Не удалось сохранить настройки или данные") } }, { error -> showError(error.message ?: "Сетевая ошибка · данные сохранены") })
            }
        }
    }
}
