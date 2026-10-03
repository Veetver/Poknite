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
    private var history: LinearLayout? = null
    private var historyScroll: ScrollView? = null
    private var sendButton: Button? = null
    private var mentionButton: Button? = null
    private var chatHint: TextView? = null
    private var lastMessages = emptyList<WireMessage>()
    private var renderedChannel = -1L
    private var renderedPage = -1
    private var quiet: Switch? = null
    private var channels = emptyList<Channel>()
    private var selected = 0L
    private var composerChannel = 0L
    private var page = 0
    private var binding = false
    private var busy = false
    private var registered = false
    private var profileSnapshot = ""
    private var administration: NativeManagement? = null
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
        if(Build.VERSION.SDK_INT>=30) window.setDecorFitsSystemWindows(false)
        window.setSoftInputMode(android.view.WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE)
        selected = savedInstanceState?.getLong("selected") ?: app.settings.selectedConversation
        render()
    }
    override fun onSaveInstanceState(out: Bundle) { out.putLong("selected", selected); super.onSaveInstanceState(out) }
    private fun render() {
        saveComposer()
        handler.removeCallbacks(save)
        composer = null; composerChannel = 0; picker = null; history = null; quiet = null; historyScroll=null;sendButton=null;mentionButton=null;chatHint=null
        renderedChannel=-1;renderedPage=-1;lastMessages=emptyList()
        root = vertical()
        val content:View=if(app.settings.token.isEmpty()) ScrollView(this).apply {addView(root)} else root
        content.setOnApplyWindowInsetsListener { v, insets ->
            val horizontal = if (v === root) dp(12) else 0
            val vertical = if (v === root) dp(6) else 0
            if (Build.VERSION.SDK_INT >= 30) {
                val padding = insets.getInsets(android.view.WindowInsets.Type.systemBars() or android.view.WindowInsets.Type.ime())
                v.setPadding(padding.left + horizontal, padding.top + vertical, padding.right + horizontal, padding.bottom + vertical)
            } else v.setPadding(insets.systemWindowInsetLeft + horizontal, insets.systemWindowInsetTop + vertical, insets.systemWindowInsetRight + horizontal, insets.systemWindowInsetBottom + vertical)
            insets
        }
        setContentView(content)
        profileSnapshot=app.settings.profile
        status = text(app.settings.status,13f)
        battery = text("").apply { visibility=View.GONE }
        if (app.settings.token.isEmpty()) {root.addView(text("Poknite",26f));root.addView(status);setup()} else connected()
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
            work({ JSONObject(app.request("/v2/devices/enroll", "POST", JSONObject().put("invitation", code).put("device_name", deviceName), address, false)) }) { result ->
                app.settings.enabled = false; stopService(Intent(this, ConnectionService::class.java))
                app.store.resetAccount()
                app.settings.profile = JSONObject().put("id",result.getLong("user_id")).put("name",result.getString("user_name")).toString(); app.settings.contacts = "[]"; app.settings.endpoint = address; app.settings.userId = result.getLong("user_id"); app.settings.deviceId = result.getLong("device_id"); app.settings.token = result.getString("token")
                app.settings.status = "Устройство подключено"
                selected = 0; app.settings.selectedConversation=0; render(); enable()
            }
        }
        root.addView(enroll)
        root.addView(button("Проверить DNS, HTTPS и WebSocket") { val raw=endpoint.text.toString();work({ app.diagnostics(raw) }) { report -> AlertDialog.Builder(this).setTitle("Проверка соединения").setMessage(report).setPositiveButton("Закрыть",null).show() } })
        root.addView(text("Приглашение выдаёт администратор. Оно действует 15 минут и используется один раз."))
        if (BuildConfig.DEBUG) root.addView(text("Тестовая сборка: HTTP разрешён только для localhost и адреса эмулятора 10.0.2.2."))
    }
    private fun connected() {
        root.setPadding(dp(12),dp(6),dp(12),dp(6))
        root.setBackgroundColor(android.graphics.Color.rgb(245,247,248))
        val toolbar=LinearLayout(this).apply {gravity=android.view.Gravity.CENTER_VERTICAL}
        toolbar.addView(text("Poknite",21f),LinearLayout.LayoutParams(0,-2,1f))
        toolbar.addView(button("Контакты") { contacts() })
        toolbar.addView(button("Настройки") { options() })
        root.addView(toolbar)
        val reception=LinearLayout(this).apply {gravity=android.view.Gravity.CENTER_VERTICAL}
        reception.addView(status,LinearLayout.LayoutParams(0,-2,1f))
        reception.addView(button("Приём") {if(app.settings.enabled)disable() else enable()})
        root.addView(reception)
        if(profileActions().any {it in setOf("users","channels","devices","invitations","roles","manage_channel")}) root.addView(button("Управление") {NativeManagement(this,app).also {administration=it}.open()})
        channels=app.store.channels()
        val channelRow=LinearLayout(this).apply {gravity=android.view.Gravity.CENTER_VERTICAL}
        picker=Spinner(this).also {s ->
            s.contentDescription="Выбрать канал или личный диалог"
            channelRow.addView(s,LinearLayout.LayoutParams(0,dp(48),1f))
            s.onItemSelectedListener=object:AdapterView.OnItemSelectedListener {
                override fun onNothingSelected(parent:AdapterView<*>?)=Unit
                override fun onItemSelected(parent:AdapterView<*>?,view:View?,position:Int,id:Long){
                    if(binding || position !in channels.indices || channels[position].id==selected)return
                    saveComposer();selected=channels[position].id;app.settings.selectedConversation=selected;page=0;loadChannel()
                }
            }
        }
        quiet=Switch(this).apply {text="Тихо";contentDescription="Без звука в этом разговоре";setOnCheckedChangeListener {_,value -> if(!binding && selected>0)app.store.setQuiet(selected,value)}}.also {channelRow.addView(it)}
        root.addView(channelRow)
        chatHint=text("",12f).also {root.addView(it)}
        val pages=LinearLayout(this)
        pages.addView(button("Ранее") {if(app.store.history(selected,(page+1)*50).isNotEmpty()){page++;loadHistory()}},LinearLayout.LayoutParams(0,-2,1f))
        pages.addView(button("К последним ↓") {page=0;renderedPage=-1;loadHistory()},LinearLayout.LayoutParams(0,-2,1f))
        root.addView(pages)
        history=LinearLayout(this).apply {orientation=LinearLayout.VERTICAL;setPadding(0,dp(8),0,dp(8))}
        historyScroll=ScrollView(this).apply {isFillViewport=true;addView(history)}.also {root.addView(it,LinearLayout.LayoutParams(-1,0,1f))}
        val compose=LinearLayout(this).apply {orientation=LinearLayout.VERTICAL;setPadding(0,dp(6),0,0)}
        composer = EditText(this).apply {
            hint = "Сообщение · до 4096 байт"
            minLines = 1; maxLines = 3
            inputType = android.text.InputType.TYPE_CLASS_TEXT or android.text.InputType.TYPE_TEXT_FLAG_MULTI_LINE or android.text.InputType.TYPE_TEXT_FLAG_CAP_SENTENCES
            filters = arrayOf(InputFilter { source, start, end, dest, dstart, dend ->
                val candidate = dest.subSequence(0, dstart).toString() + source.subSequence(start, end) + dest.subSequence(dend, dest.length)
                if (candidate.toByteArray().size > 4096 || candidate.any { it.isISOControl() && it !in "\n\r\t" }) "" else null
            })
            imeOptions=android.view.inputmethod.EditorInfo.IME_ACTION_SEND
            setOnEditorActionListener {_,action,_ -> if(action==android.view.inputmethod.EditorInfo.IME_ACTION_SEND){publish();true}else false}
            setOnKeyListener {_,code,event -> if(code==android.view.KeyEvent.KEYCODE_ENTER && event.isCtrlPressed){if(event.action==android.view.KeyEvent.ACTION_DOWN)publish();true}else false}
            addTextChangedListener(object : TextWatcher {
                override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) = Unit
                override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) { if (!binding) { handler.removeCallbacks(save); handler.postDelayed(save, 400); updateComposerControls(); if (count==1 && s?.get(start)=='@' && channels.firstOrNull { it.id == selected }?.actions?.contains("mention") == true) mention(start+1) } }
                override fun afterTextChanged(s: Editable?) = Unit
            })
        }.also {compose.addView(it)}
        val actions=LinearLayout(this)
        mentionButton=button("@") {mention()}.apply {contentDescription="Упомянуть участника"}.also {actions.addView(it,LinearLayout.LayoutParams(dp(54),dp(44)))}
        sendButton=button("Отправить") {publish()}.also {actions.addView(it,LinearLayout.LayoutParams(0,dp(44),1f))}
        compose.addView(actions);root.addView(compose)
        refreshChannels()
    }
    private fun refreshChannels() {
        val current = app.store.channels()
        if (current == channels && picker?.adapter != null) return
        saveComposer()
        channels = current
        binding = true
        picker?.adapter = ArrayAdapter(this, android.R.layout.simple_spinner_dropdown_item, channels.map { (if(it.kind=="direct") "Лично: " else "") + it.name + if(it.closed) " (закрыт)" else "" })
        if (channels.none { it.id == selected }) {
            selected=channels.firstOrNull { it.id==app.settings.selectedConversation }?.id ?: channels.firstOrNull()?.id ?: 0
            page=0
        }
        if(selected>0)app.settings.selectedConversation=selected
        picker?.setSelection(channels.indexOfFirst { it.id == selected }.coerceAtLeast(0))
        binding = false
        loadChannel()
    }
    private fun loadChannel() {
        handler.removeCallbacks(save)
        binding = true
        composer?.setText(app.store.draft(selected)?.text ?: "")
        composerChannel = selected
        composer?.isEnabled = channels.firstOrNull { it.id==selected }?.actions?.contains("send") == true
        quiet?.isChecked = app.store.quiet(selected)
        binding = false
        renderedChannel=-1
        updateComposerControls()
        loadHistory()
    }
    private fun loadHistory() {
        val area=history ?: return
        val messages=app.store.history(selected,page*50).asReversed()
        if(renderedChannel==selected && renderedPage==page && lastMessages==messages)return
        val changedPage=renderedChannel!=selected || renderedPage!=page
        val scroller=historyScroll
        val atEnd=scroller==null || scroller.scrollY+scroller.height >= (scroller.getChildAt(0)?.height ?: 0)-dp(24)
        renderedChannel=selected;renderedPage=page;lastMessages=messages
        area.removeAllViews()
        val format=DateFormat.getDateTimeInstance(DateFormat.SHORT,DateFormat.SHORT)
        if(messages.isEmpty())area.addView(text(if(selected==0L) "Выберите разговор. Список появится после подключения." else "Здесь пока нет сообщений. Начните разговор.",14f))
        for(m in messages) {
            val own=m.senderId==app.settings.userId
            val row=LinearLayout(this).apply {gravity=if(own)android.view.Gravity.END else android.view.Gravity.START;setPadding(0,dp(4),0,dp(4))}
            val content=android.text.SpannableStringBuilder().append(coloredNickname(m.senderName,m.senderColor))
            val metaStart=content.length
            content.append(if(own) " · вы" else "").append(" · ${format.format(Date(m.createdAt*1000))}")
            content.setSpan(android.text.style.RelativeSizeSpan(0.8f),metaStart,content.length,android.text.Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            content.append("\n").append(m.text)
            val bubble=TextView(this).apply {text=content;textSize=16f;setTextColor(android.graphics.Color.rgb(31,42,43));setTextIsSelectable(true);maxWidth=(resources.displayMetrics.widthPixels*0.86f).toInt();setPadding(dp(12),dp(10),dp(12),dp(10));background=android.graphics.drawable.GradientDrawable().apply {cornerRadius=dp(12).toFloat();setColor(if(own)android.graphics.Color.rgb(229,242,235) else android.graphics.Color.WHITE);setStroke(dp(1),android.graphics.Color.rgb(221,228,228))}}
            row.addView(bubble);area.addView(row)
        }
        if(changedPage || atEnd)scroller?.post {scroller.fullScroll(View.FOCUS_DOWN)}
    }
    private fun updateComposerControls() {
        val current=channels.firstOrNull {it.id==selected}
        val canSend=current?.actions?.contains("send")==true
        composer?.isEnabled=canSend
        sendButton?.isEnabled=canSend && app.online && !busy && validText(composer?.text.toString())
        mentionButton?.isEnabled=app.online && !busy && current?.actions?.contains("mention")==true
        chatHint?.text=when {current==null->"Выберите канал или личный диалог";current.closed->"Канал закрыт · доступна история";!canSend->"Только чтение · отправка недоступна";!app.online->"Нет связи · черновик сохраняется";current.kind=="direct"->"Личный диалог · только вы и собеседник";else->"@ — выбрать адресата уведомления"}
    }
    private fun saveComposer() { if (!binding && selected > 0 && composerChannel == selected) composer?.let { app.store.saveDraft(selected, it.text.toString()) } }
    private fun publish() {
        if (busy || selected == 0L) return
        if(channels.firstOrNull { it.id==selected }?.actions?.contains("send") != true) {showError("Нет права отправки в этот разговор");return;}
        val value = composer?.text.toString()
        if (!validText(value)) { showError("Нужен непустой текст до 4096 байт UTF-8"); return }
        saveComposer()
        if (!app.online) { showError("Сейчас нет связи. Черновик сохранён; отправьте после подключения."); return }
        val draft = app.store.draft(selected) ?: return
        work({
            val members=JSONArray(app.request("/v2/conversations/${draft.channelId}/e2ee-members"))
            val encrypted=app.e2ee.seal(draft,members)
            val response=JSONObject(app.request("/v2/conversations/${draft.channelId}/messages", "POST", encrypted))
            require(response.getString("text")==encrypted.getString("text") && response.getLong("channel_id")==draft.channelId && response.getLong("sender_id")==app.settings.userId) {"Сервер изменил отправленное сообщение"}
            val m=app.e2ee.decodeWire(response)
            require(m.verified) {"Не удалось проверить отправленное сообщение"}
            app.store.receive(m, false, app.settings.userId, false)
            app.store.sentDraft(draft)
        }) {
            if (selected == draft.channelId && composer?.text.toString() == draft.text) { binding = true; composer?.setText(""); binding = false }
            if (selected == draft.channelId) { page=0;renderedPage=-1;loadHistory() }
            updateComposerControls()
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
        AlertDialog.Builder(this).setTitle("Настройки").setItems(arrayOf("Мои устройства", "Фоновая работа и батарея", "Разрешения уведомлений", "Очистить локальную историю", "Новое подключение", "Мой ник и цвет", "Адрес управления", "Шифрование текущего разговора")) { _, i ->
            when (i) {
                0 -> devices()
                7 -> encryption()
                1 -> batterySetup()
                2 -> startActivity(Intent(SystemSettings.ACTION_APP_NOTIFICATION_SETTINGS).putExtra(SystemSettings.EXTRA_APP_PACKAGE, packageName))
                3 -> AlertDialog.Builder(this).setMessage("Удалить полученные сообщения на этом устройстве?").setNegativeButton("Отмена", null).setPositiveButton("Очистить") { _, _ -> app.store.clearHistory(); val nm = getSystemService(android.app.NotificationManager::class.java); nm.activeNotifications.filter { it.tag != null }.forEach { nm.cancel(it.tag, it.id) }; page = 0; loadHistory() }.show()
                5 -> nickname()
                6 -> NativeManagement(this,app).configure()
                4 -> AlertDialog.Builder(this).setMessage("Сбросить подключение и локальную историю? Для подключения потребуется новое приглашение. Старое устройство можно отозвать через другое своё устройство.").setNegativeButton("Отмена", null).setPositiveButton("Сбросить") { _, _ -> disable(); app.settings.token = ""; app.store.resetAccount(); selected = 0; app.settings.selectedConversation=0; render() }.show()
            }
        }.show()
    }
    private fun profileActions(): List<String> = JSONObject(app.settings.profile).optJSONArray("actions")?.let { a -> (0 until a.length()).map { a.getString(it) } } ?: emptyList()
    private fun nickname() {
        val input=EditText(this).apply { setText(JSONObject(app.settings.profile).optString("name"));setSingleLine(true) };val color=EditText(this).apply { hint="RGB: 0…255,0…255,0…255";setText(rgbString(JSONObject(app.settings.profile).optString("color","#808080")));setSingleLine(true) };val box=vertical().apply { addView(input);addView(color) }
        AlertDialog.Builder(this).setTitle("Мой ник и цвет RGB").setView(box).setNegativeButton("Отмена",null).setPositiveButton("Сохранить") { _,_ -> work({ app.request("/v2/profile","PUT",JSONObject().put("name",input.text.toString().trim()).put("color",rgbColor(color.text.toString()))) }) { user -> app.settings.profile=user;app.store.updateUsers(JSONArray().put(JSONObject(user)));render() } }.show()
    }
    private fun contacts() {
        work({ JSONArray(app.request("/v2/contacts")) }) { users ->
            AlertDialog.Builder(this).setTitle("Контакты").setItems(Array<CharSequence>(users.length()) { users.getJSONObject(it).let { u -> coloredNickname(u.getString("name"),u.optString("color","#808080")) } }) { _,i ->
                val id=users.getJSONObject(i).getLong("id")
                work({ JSONObject(app.request("/v2/conversations/direct","POST",JSONObject().put("user_id",id))) }) { c ->
                    saveComposer();selected=c.getLong("id");app.settings.selectedConversation=selected;page=0;work({ JSONArray(app.request("/v2/conversations")) }) { all -> app.store.updateChannels(all);refreshChannels();loadChannel() }
                }
            }.setNegativeButton("Закрыть",null).show()
        }
    }
    private fun mention(caret:Int=composer?.selectionStart ?: 0) {
        val channel=selected;saveComposer()
        work({ JSONArray(app.request("/v2/conversations/$channel/participants")) }) { users ->
            if(channel!=selected) return@work
            AlertDialog.Builder(this).setTitle("Упомянуть участника").setItems(Array<CharSequence>(users.length()) { users.getJSONObject(it).let { u -> coloredNickname(u.getString("name"),u.optString("color","#808080")) } }) { _,i ->
                if(channel!=selected) return@setItems
                val user=users.getJSONObject(i);val before=composer?.text.toString()
                if(caret<0 || caret>before.length)return@setItems
                val byteStart=if(caret>0 && before[caret-1]=='@')caret-1 else caret
                val start=before.codePointCount(0,byteStart);val insertion="@"+user.getString("name")+" "
                val value=before.substring(0,byteStart)+insertion+before.substring(caret)
                val end=start+insertion.codePointCount(0,insertion.length)-1
                val old=JSONArray(adjustMentions(before,value,app.store.draft(channel)?.mentions ?: "[]"));val all=(0 until old.length()).map { old.getJSONObject(it) }.toMutableList()
                all+=JSONObject().put("user_id",user.getLong("id")).put("start",start).put("end",end)
                val mentions=JSONArray();all.sortedBy { it.getInt("start") }.forEach { mentions.put(it) }
                runCatching { app.store.saveDraft(channel,value,mentions.toString()) }.onSuccess { binding=true;composer?.setText(value);composer?.setSelection(byteStart+insertion.length);binding=false }.onFailure { showError(it.message ?: "Текст слишком длинный") }
            }.setNegativeButton("Отмена",null).show()
        }
    }
    private fun encryption() {
        val channel=selected
        if(channel<=0){showError("Выберите разговор");return}
        work({JSONArray(app.request("/v2/conversations/$channel/e2ee-members"))}) { members ->
            val layout=LinearLayout(this).apply {orientation=LinearLayout.VERTICAL;setPadding(dp(16),dp(12),dp(16),dp(12))}
            layout.addView(TextView(this).apply {text="Создайте ключ на одном устройстве и передайте код только доверенным участникам через личную встречу или другой защищённый канал. Сервер код не получает. При изменении состава устройств смените ключ."})
            val fingerprint=TextView(this).apply {setTextIsSelectable(true);text=runCatching {"Отпечаток: "+app.e2ee.fingerprint(channel)}.getOrDefault("Ключ ещё не подключён")}
            layout.addView(fingerprint)
            val code=EditText(this).apply {hint="Код ключа с доверенного устройства";minLines=3;maxLines=5;inputType=android.text.InputType.TYPE_CLASS_TEXT or android.text.InputType.TYPE_TEXT_FLAG_MULTI_LINE or android.text.InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS}
            layout.addView(code)
            layout.addView(button("Создать / сменить ключ") {
                AlertDialog.Builder(this).setTitle("Новый ключ").setMessage("Новый код нужно перенести на все доверенные устройства. Уже сохранённая история останется доступна.").setNegativeButton("Отмена",null).setPositiveButton("Создать") {_,_->
                    work({app.e2ee.createKey(channel,members)}) {token->code.setText(token);fingerprint.text="Отпечаток: "+app.e2ee.fingerprint(channel);app.changed()}
                }.show()
            })
            layout.addView(button("Показать код ключа") {work({app.e2ee.exportKey(channel)}) {code.setText(it)}})
            layout.addView(button("Подключить код") {val token=code.text.toString();work({app.e2ee.importKey(channel,members,token)}) {fingerprint.text="Ключ подключён: "+app.e2ee.fingerprint(channel);app.changed();if(selected==channel){renderedPage=-1;loadHistory()}}})
            AlertDialog.Builder(this).setTitle("Шифрование разговора").setView(layout).setNegativeButton("Закрыть",null).show()
        }
    }
    private fun devices() {
        work({ JSONArray(app.request("/v2/devices")) }) { devices ->
            val names = Array(devices.length()) { i -> devices.getJSONObject(i).let { it.getString("name") + if (it.getBoolean("current")) " (это устройство)" else "" } }
            AlertDialog.Builder(this).setTitle("Выберите устройство для отзыва").setItems(names) { _, i ->
                val d = devices.getJSONObject(i)
                AlertDialog.Builder(this).setMessage("Отозвать «${d.getString("name") }»?").setNegativeButton("Отмена", null).setPositiveButton("Отозвать") { _, _ ->
                    work({ app.request("/v2/devices/${d.getLong("id")}", "DELETE") }) { if (d.getBoolean("current")) { disable(); app.settings.token = ""; app.settings.status = "Устройство отозвано"; app.store.resetAccount(); selected = 0; app.settings.selectedConversation=0; render() } else Toast.makeText(this, "Устройство отозвано", Toast.LENGTH_SHORT).show() }
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
        updateComposerControls()
        battery.text = if (getSystemService(PowerManager::class.java).isIgnoringBatteryOptimizations(packageName)) "Батарея: фоновая работа разрешена" else "Батарея: действует оптимизация Android"
    }
    private fun refresh() {
        if(profileSnapshot!=app.settings.profile){saveComposer();administration?.close();administration=null;render();return;}
        if (isFinishing || isDestroyed) return; updateStatus(); refreshChannels(); loadHistory() }
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
        updateComposerControls()
        app.io.execute {
            val result = runCatching(job)
            runOnUiThread {
                busy = false
                updateComposerControls()
                if (!isFinishing && !isDestroyed) result.fold({ value -> runCatching { done(value) }.onFailure { showError("Не удалось сохранить настройки или данные") } }, { error -> showError(error.message ?: "Сетевая ошибка · данные сохранены") })
            }
        }
    }
}
