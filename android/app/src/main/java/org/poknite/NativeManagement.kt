package org.poknite

import android.app.Activity
import android.app.AlertDialog
import android.os.Handler
import android.os.Looper
import android.widget.*
import org.json.JSONArray
import org.json.JSONObject

class NativeManagement(private val activity: Activity, private val app: PokniteApp) {
    private val main=Handler(Looper.getMainLooper())
    private val dialogs=mutableListOf<AlertDialog>()
    private fun show(builder: AlertDialog.Builder) { dialogs+=builder.show() }
    fun close() { dialogs.forEach { it.dismiss() }; dialogs.clear() }
    private fun actions(): List<String> = JSONObject(app.settings.profile).optJSONArray("actions")?.let { a -> (0 until a.length()).map { a.getString(it) } } ?: emptyList()
    private fun work(action: () -> String, done: (String) -> Unit) {
        app.io.execute { val result=runCatching(action);main.post { if(activity.isFinishing || activity.isDestroyed) return@post;result.onSuccess(done).onFailure { show(AlertDialog.Builder(activity).setTitle("Poknite").setMessage(it.message ?: "Управление недоступно из текущей сети").setPositiveButton("Закрыть",null)) } } }
    }
    private fun request(path: String,method: String="GET",body: Any?=null,done:(String)->Unit={ Toast.makeText(activity,"Изменения сохранены",Toast.LENGTH_SHORT).show() }) { work({ app.adminRequest(path,method,body) },done) }
    private fun layout()=LinearLayout(activity).apply { orientation=LinearLayout.VERTICAL;setPadding(24,8,24,8) }
    private fun input(hint: String,value: String="")=EditText(activity).apply { this.hint=hint;setText(value);setSingleLine(true) }
    fun configure() {
        val box=layout();val endpoint=input("HTTPS-адрес управления",app.settings.managementEndpoint);val ip=input("IP подключения (необязательно)",app.settings.managementIp);box.addView(endpoint);box.addView(ip)
        show(AlertDialog.Builder(activity).setTitle("Адрес управления").setView(box).setNegativeButton("Отмена",null).setPositiveButton("Сохранить") { _,_ ->
            runCatching { val address=canonicalEndpoint(endpoint.text.toString());val raw=ip.text.toString().trim();require(raw.isEmpty() || raw.matches(Regex("[0-9.]+|[0-9a-fA-F:]+")));app.settings.managementEndpoint=address;app.settings.managementIp=raw }.onFailure { Toast.makeText(activity,it.message ?: "Проверьте адрес и IP",Toast.LENGTH_LONG).show() }
        })
    }
    fun open() {
        val rights=actions();val choices=mutableListOf<Pair<String,String>>()
        if(rights.any { it in setOf("users","devices","invitations") }) choices+="Пользователи" to "users"
        if(rights.any { it in setOf("channels","manage_channel") }) choices+="Каналы" to "channels"
        if("roles" in rights) choices+="Роли" to "roles"
        choices+="Адрес управления" to "settings"
        show(AlertDialog.Builder(activity).setTitle("Управление").setItems(choices.map { it.first }.toTypedArray()) { _,i -> val entity=choices[i].second;if(entity=="settings")configure() else list(entity) }.setNegativeButton("Закрыть",null))
    }
    private fun list(entity: String) {
        request("/v2/admin/$entity") { result ->
            val rows=JSONArray(result)
            show(AlertDialog.Builder(activity).setTitle(label(entity)).setItems(Array(rows.length()) { i -> rows.getJSONObject(i).let { it.getString("name") + if(it.optBoolean("disabled") || it.optBoolean("closed")) " (отключён / закрыт)" else "" } }) { _,i -> details(entity,rows.getJSONObject(i)) }.setPositiveButton(if(entity in actions()) "Создать" else "Обновить") { _,_ -> if(entity in actions()) edit(entity,null) else list(entity) }.setNegativeButton("Закрыть",null))
        }
    }
    private fun details(entity: String,row: JSONObject) {
        val choices=mutableListOf<Pair<String,Int>>()
        val rights=actions()
        if(entity in rights || entity=="channels" && "manage_channel" in rights) {choices+="Изменить" to 0;choices+="Отключить / закрыть" to 1}
        if(entity=="users") {if("invitations" in rights){choices+="Приглашение" to 2;choices+="Отозвать приглашения" to 4};if("devices" in rights) choices+="Устройства" to 3}
        if(entity=="channels") choices+="Правила доступа" to 2
        show(AlertDialog.Builder(activity).setTitle(row.getString("name")).setItems(choices.map { it.first }.toTypedArray()) { _,i ->
            val id=row.getLong("id")
            when(choices[i].second) {
                0 -> edit(entity,row)
                1 -> show(AlertDialog.Builder(activity).setMessage("Отключить или закрыть? История сохраняется.").setNegativeButton("Отмена",null).setPositiveButton("Продолжить") { _,_ -> request("/v2/admin/$entity/$id","DELETE") })
                2 -> if(entity=="channels") rules(id) else request("/v2/admin/users/$id/invitations","POST") { result -> val code=JSONObject(result).getString("invitation");val text=TextView(activity).apply { this.text=code;setTextIsSelectable(true);setPadding(24,16,24,16) };show(AlertDialog.Builder(activity).setTitle("Приглашение · 15 минут").setView(text).setPositiveButton("Закрыть",null)) }
                3 -> request("/v2/admin/users/$id/devices") { result -> val devices=JSONArray(result);show(AlertDialog.Builder(activity).setTitle("Отключить устройство").setItems(Array(devices.length()) { devices.getJSONObject(it).getString("name") }) { _,d -> val device=devices.getJSONObject(d);show(AlertDialog.Builder(activity).setMessage("Отключить ${device.getString("name")}? ").setNegativeButton("Отмена",null).setPositiveButton("Отключить") { _,_ -> request("/v2/admin/devices/${device.getLong("id")}","DELETE") }) }.setNegativeButton("Закрыть",null)) }
                4 -> request("/v2/admin/users/$id/invitations","DELETE")
            }
        }.setNegativeButton("Закрыть",null))
    }
    private fun edit(entity: String,row: JSONObject?) {
        if(entity=="users") request("/v2/admin/roles") { roles -> form(entity,row,JSONArray(roles)) } else form(entity,row,JSONArray())
    }
    private fun form(entity: String,row: JSONObject?,roles: JSONArray) {
        val box=layout();val name=input(if(entity=="users") "Уникальный ник" else "Название",row?.optString("name") ?: "");box.addView(name)
        val color=input("RGB: случайный, если поле пустое",row?.optString("color")?.let { rgbString(it) } ?: "");if(entity=="users")box.addView(color)
        val disabled=CheckBox(activity).apply { text=if(entity=="users") "Пользователь отключён" else "Канал закрыт";isChecked=row?.optBoolean(if(entity=="users") "disabled" else "closed") ?: false }
        if(entity!="roles") box.addView(disabled)
        val assigned=mutableListOf<Pair<Long,CheckBox>>();val selectors=mutableListOf<Pair<String,Spinner>>()
        if(entity=="users") for(i in 0 until roles.length()) {val r=roles.getJSONObject(i);val id=r.getLong("id");val c=CheckBox(activity).apply { text=r.getString("name");isChecked=row?.optJSONArray("roles")?.let { a -> (0 until a.length()).any { a.getLong(it)==id } } ?: (id==2L) };box.addView(c);assigned+=id to c}
        if(entity=="roles") for((key,title) in permissions) {val label=TextView(activity).apply { text=title };box.addView(label);val spinner=Spinner(activity).apply { adapter=ArrayAdapter(activity,android.R.layout.simple_spinner_dropdown_item,arrayOf("Не задано","Разрешить","Запретить"));setSelection(if(contains(row?.optJSONArray("deny"),key))2 else if(contains(row?.optJSONArray("allow"),key))1 else 0) };box.addView(spinner);selectors+=key to spinner}
        val scroll=ScrollView(activity).apply { addView(box) }
        show(AlertDialog.Builder(activity).setTitle(label(entity)).setView(scroll).setNegativeButton("Отмена",null).setPositiveButton("Сохранить") { _,_ ->
            val body=JSONObject().put("name",name.text.toString().trim())
            when(entity) {"users" -> body.put("disabled",disabled.isChecked).put("roles",JSONArray(assigned.filter { it.second.isChecked }.map { it.first }));"channels" -> body.put("closed",disabled.isChecked);else -> body.put("allow",JSONArray(selectors.filter { it.second.selectedItemPosition==1 }.map { it.first })).put("deny",JSONArray(selectors.filter { it.second.selectedItemPosition==2 }.map { it.first }))}
            if(entity=="users" && color.text.isNotBlank()) {runCatching { body.put("color",rgbColor(color.text.toString())) }.onFailure { Toast.makeText(activity,it.message,Toast.LENGTH_LONG).show();return@setPositiveButton }}
            val id=row?.getLong("id");request("/v2/admin/$entity"+(if(id==null) "" else "/$id"),if(id==null)"POST" else "PUT",body)
        })
    }
    private fun rules(channel: Long) {
        request("/v2/admin/roles") { result -> val roles=JSONArray(result);show(AlertDialog.Builder(activity).setTitle("Выберите роль").setItems(Array(roles.length()) { roles.getJSONObject(it).getString("name") }) { _,index -> val role=roles.getJSONObject(index);request("/v2/admin/channels/$channel/rules") { resultRules -> ruleForm(channel,role.getLong("id"),JSONArray(resultRules)) } }.setNegativeButton("Закрыть",null)) }
    }
    private fun ruleForm(channel: Long,role: Long,old: JSONArray) {
        val row=(0 until old.length()).map { old.getJSONObject(it) }.firstOrNull { it.getLong("role_id")==role };val box=layout();val choices=mutableListOf<Pair<String,Spinner>>()
        for((key,title) in permissions.filter { it.first in setOf("read","send","mention","manage_channel") }) {box.addView(TextView(activity).apply { text=title });val s=Spinner(activity).apply { adapter=ArrayAdapter(activity,android.R.layout.simple_spinner_dropdown_item,arrayOf("Не задано","Разрешить","Запретить"));setSelection(if(contains(row?.optJSONArray("deny"),key))2 else if(contains(row?.optJSONArray("allow"),key))1 else 0) };box.addView(s);choices+=key to s}
        show(AlertDialog.Builder(activity).setTitle("Правила роли").setView(box).setNegativeButton("Отмена",null).setPositiveButton("Сохранить") { _,_ -> val all=JSONArray();for(i in 0 until old.length())if(old.getJSONObject(i).getLong("role_id")!=role)all.put(old.getJSONObject(i));all.put(JSONObject().put("role_id",role).put("allow",JSONArray(choices.filter { it.second.selectedItemPosition==1 }.map { it.first })).put("deny",JSONArray(choices.filter { it.second.selectedItemPosition==2 }.map { it.first })));request("/v2/admin/channels/$channel/rules","PUT",all) })
    }
    private fun contains(a: JSONArray?,key: String)=a!=null && (0 until a.length()).any { a.getString(it)==key }
    private fun label(entity: String)=when(entity){"users"->"Пользователи";"channels"->"Каналы";else->"Роли"}
    companion object { val permissions=listOf("profile" to "Изменение профиля","contacts" to "Контакты","direct" to "Личная переписка","read" to "Чтение","send" to "Отправка","mention" to "Упоминания","manage_channel" to "Управление каналом","users" to "Пользователи","channels" to "Каналы","devices" to "Устройства","invitations" to "Приглашения","roles" to "Роли") }
}
