package org.poknite

import android.content.ContentValues
import android.content.Context
import android.database.sqlite.SQLiteDatabase
import android.database.sqlite.SQLiteOpenHelper
import org.json.JSONArray

class LocalStore(context: Context, name: String = "history.db") : SQLiteOpenHelper(context, name, null, 3) {
    override fun onConfigure(db: SQLiteDatabase) {
        db.setForeignKeyConstraintsEnabled(true)
        db.rawQuery("PRAGMA journal_mode=DELETE", null).use { check(it.moveToFirst()) }
        db.rawQuery("PRAGMA max_page_count=2048", null).use { check(it.moveToFirst() && it.getLong(0) <= 2048) }
        db.execSQL("PRAGMA synchronous=FULL")
    }
    override fun onCreate(db: SQLiteDatabase) {
        db.execSQL("CREATE TABLE meta(key TEXT PRIMARY KEY,value INTEGER NOT NULL)")
        db.execSQL("INSERT INTO meta VALUES('cursor',0)")
        db.execSQL("CREATE TABLE channels(id INTEGER PRIMARY KEY,name TEXT NOT NULL,quiet INTEGER NOT NULL DEFAULT 0,details TEXT NOT NULL DEFAULT '{}')")
        db.execSQL("CREATE TABLE messages(id TEXT PRIMARY KEY,seq INTEGER NOT NULL,channel_id INTEGER NOT NULL,sender_id INTEGER NOT NULL,sender_name TEXT NOT NULL,text TEXT NOT NULL,created_at INTEGER NOT NULL,expires_at INTEGER NOT NULL,mentions TEXT NOT NULL DEFAULT '[]',sender_color TEXT NOT NULL DEFAULT '#808080')")
        db.execSQL("CREATE INDEX history ON messages(channel_id,seq DESC)")
        db.execSQL("CREATE INDEX ordering ON messages(seq)")
        db.execSQL("CREATE TABLE notices(id TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,replay INTEGER NOT NULL)")
        db.execSQL("CREATE TABLE drafts(channel_id INTEGER PRIMARY KEY,text TEXT NOT NULL,client_id TEXT NOT NULL,mentions TEXT NOT NULL DEFAULT '[]')")
    }
    override fun onUpgrade(db: SQLiteDatabase, oldVersion: Int, newVersion: Int) {
        if (oldVersion < 2) {
            db.execSQL("ALTER TABLE channels ADD COLUMN details TEXT NOT NULL DEFAULT '{}'")
            db.execSQL("ALTER TABLE messages ADD COLUMN mentions TEXT NOT NULL DEFAULT '[]'")
            db.execSQL("ALTER TABLE drafts ADD COLUMN mentions TEXT NOT NULL DEFAULT '[]'")
        }
        if (oldVersion < 3) db.execSQL("ALTER TABLE messages ADD COLUMN sender_color TEXT NOT NULL DEFAULT '#808080'")
    }
    override fun onOpen(db: SQLiteDatabase) {
        super.onOpen(db)
        db.execSQL("CREATE TABLE IF NOT EXISTS e2ee_pending(id TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,wire TEXT NOT NULL)")
        db.execSQL("CREATE TABLE IF NOT EXISTS e2ee_drafts(aud TEXT NOT NULL,cid INTEGER NOT NULL,mid TEXT NOT NULL,source_hash TEXT NOT NULL,sealed TEXT NOT NULL,PRIMARY KEY(aud,cid))")
    }
    @Synchronized fun sealedDraft(aud: String, cid: Long): List<String>? = readableDatabase.rawQuery("SELECT mid,source_hash,sealed FROM e2ee_drafts WHERE aud=? AND cid=?",arrayOf(aud,cid.toString())).use { if(it.moveToFirst()) listOf(it.getString(0),it.getString(1),it.getString(2)) else null }
    @Synchronized fun saveSealedDraft(aud: String, cid: Long, mid: String, hash: String, sealed: String) {
        writableDatabase.execSQL("INSERT OR REPLACE INTO e2ee_drafts VALUES(?,?,?,?,?)",arrayOf<Any>(aud,cid,mid,hash,sealed))
    }
    @Synchronized fun rotateDraftKey(aud: String, cid: Long) {
        val db=writableDatabase;db.beginTransaction()
        try {db.execSQL("UPDATE drafts SET client_id=? WHERE channel_id=?",arrayOf<Any>(newClientId(),cid));db.delete("e2ee_drafts","aud=? AND cid=?",arrayOf(aud,cid.toString()));db.setTransactionSuccessful()} finally {db.endTransaction()}
    }
    @Synchronized fun unlockHistory(e2ee: E2ee, channel: Long) {
        val rows=readableDatabase.rawQuery("SELECT p.id,p.wire FROM e2ee_pending p JOIN messages m USING(id) WHERE m.channel_id=? LIMIT 1000",arrayOf(channel.toString())).use { c -> buildList { while(c.moveToNext()) add(c.getString(0) to c.getString(1)) } }
        for((id,wire) in rows) {
            val plain=e2ee.decodeWire(org.json.JSONObject(wire))
            if(!plain.verified)continue
            val db=writableDatabase;db.beginTransaction()
            try {db.execSQL("UPDATE messages SET text=?,mentions=? WHERE id=?",arrayOf(plain.text,plain.mentions,id));db.delete("e2ee_pending","id=?",arrayOf(id));db.setTransactionSuccessful()} finally {db.endTransaction()}
        }
    }
    @Synchronized fun cursor(): Long = readableDatabase.rawQuery("SELECT value FROM meta WHERE key='cursor'", null).use { it.moveToFirst(); it.getLong(0) }
    @Synchronized fun progress(cursor: Long, reset: Boolean = false) {
        require(cursor >= 0)
        writableDatabase.execSQL(if (reset) "UPDATE meta SET value=? WHERE key='cursor'" else "UPDATE meta SET value=MAX(value,?) WHERE key='cursor'", arrayOf(cursor))
    }
    // History and pending notification are committed atomically before an ACK is sent.
    @Synchronized fun receive(m: WireMessage, replay: Boolean, ownUser: Long, advanceCursor: Boolean = true, notify: Boolean = false): Boolean {
        val db = writableDatabase
        var inserted = false
        db.beginTransaction()
        try {
            val exists = db.rawQuery("SELECT 1 FROM messages WHERE id=?", arrayOf(m.id)).use { it.moveToFirst() }
            if (!exists) {
                db.execSQL("DELETE FROM messages WHERE id IN (SELECT id FROM messages ORDER BY seq DESC LIMIT -1 OFFSET 999)")
                val v = ContentValues().apply { put("id", m.id); put("seq", m.seq); put("channel_id", m.channelId); put("sender_id", m.senderId); put("sender_name", m.senderName); put("text", m.text); put("created_at", m.createdAt); put("expires_at", m.expiresAt); put("mentions",m.mentions); put("sender_color",m.senderColor) }
                db.insertOrThrow("messages", null, v)
                m.sealed?.let { db.execSQL("INSERT INTO e2ee_pending VALUES(?,?)",arrayOf(m.id,it)) }
                if (notify && m.verified && m.senderId != ownUser) db.execSQL("INSERT INTO notices VALUES(?,?)", arrayOf<Any>(m.id, if (replay) 1 else 0))
                inserted = true
            }
            // Seq is safe only for this message; Progress will also cover invisible channels.
            if (advanceCursor) db.execSQL("UPDATE meta SET value=MAX(value,?) WHERE key='cursor'", arrayOf(m.seq))
            db.setTransactionSuccessful()
        } finally { db.endTransaction() }
        return inserted
    }
    @Synchronized fun updateChannels(array: JSONArray): List<String> {
        val cancelled = mutableListOf<String>()
        require(array.length() <= 1000)
        val db = writableDatabase
        db.beginTransaction()
        try {
            val ids = mutableListOf<Long>()
            for (i in 0 until array.length()) {
                val c = array.getJSONObject(i); val id = c.getLong("id"); val name = c.getString("name")
                require(id > 0 && name.length <= 128)
                ids += id
                db.execSQL("INSERT OR IGNORE INTO channels(id,name) VALUES(?,?)", arrayOf(id, name))
                db.execSQL("UPDATE channels SET name=?,details=? WHERE id=?", arrayOf(name, c.toString(), id))
            }
            val allowed = (0 until array.length()).map { array.getJSONObject(it) }.filter { c -> c.optJSONArray("actions")?.let { a -> (0 until a.length()).any { a.getString(it) == "read" } } == true }.map { it.getLong("id") }
            val inaccessible = if (allowed.isEmpty()) "1" else "channel_id NOT IN (${allowed.joinToString(",")})"
            db.rawQuery("SELECT id FROM messages WHERE $inaccessible", null).use { c -> while(c.moveToNext()) cancelled += c.getString(0) }
            if (cancelled.isNotEmpty()) db.execSQL("DELETE FROM notices WHERE id IN (SELECT id FROM messages WHERE $inaccessible)")
            if (ids.isEmpty()) db.delete("channels", null, null)
            else db.execSQL("DELETE FROM channels WHERE id NOT IN (${ids.joinToString(",")})")
            db.setTransactionSuccessful()
        } finally { db.endTransaction() }
        return cancelled
    }
    @Synchronized fun channels(): List<Channel> = readableDatabase.rawQuery("SELECT id,name,details FROM channels ORDER BY id", null).use { c -> buildList { while (c.moveToNext()) add(Channel(c.getLong(0), c.getString(1), org.json.JSONObject(c.getString(2)).optString("kind","channel"), org.json.JSONObject(c.getString(2)).optBoolean("closed"), org.json.JSONObject(c.getString(2)).optJSONArray("actions")?.let { a -> (0 until a.length()).map { a.getString(it) } } ?: emptyList())) } }
    @Synchronized fun quiet(id: Long): Boolean = readableDatabase.rawQuery("SELECT quiet FROM channels WHERE id=?", arrayOf(id.toString())).use { it.moveToFirst() && it.getInt(0) != 0 }
    @Synchronized fun setQuiet(id: Long, quiet: Boolean) { writableDatabase.execSQL("UPDATE channels SET quiet=? WHERE id=?", arrayOf(if (quiet) 1 else 0, id)) }
    @Synchronized fun history(channel: Long, offset: Int = 0): List<WireMessage> = readableDatabase.rawQuery("SELECT id,seq,channel_id,sender_id,sender_name,text,created_at,expires_at,mentions,sender_color FROM messages WHERE channel_id=? ORDER BY seq DESC LIMIT 50 OFFSET ?", arrayOf(channel.toString(), offset.coerceIn(0, 1000).toString())).use { c -> buildList { while (c.moveToNext()) add(WireMessage(c.getString(0), c.getLong(1), c.getLong(2), c.getLong(3), c.getString(4), c.getString(5), c.getLong(6), c.getLong(7),c.getString(8),c.getString(9))) } }
    @Synchronized fun pending(replay: Boolean): List<WireMessage> = readableDatabase.rawQuery("SELECT m.id,m.seq,m.channel_id,m.sender_id,m.sender_name,m.text,m.created_at,m.expires_at,m.mentions,m.sender_color FROM notices n JOIN messages m ON m.id=n.id WHERE n.replay=? ORDER BY m.seq LIMIT 1000", arrayOf(if (replay) "1" else "0")).use { c -> buildList { while (c.moveToNext()) add(WireMessage(c.getString(0), c.getLong(1), c.getLong(2), c.getLong(3), c.getString(4), c.getString(5), c.getLong(6), c.getLong(7),c.getString(8),c.getString(9))) } }
    @Synchronized fun shown(ids: List<String>) { val db = writableDatabase; db.beginTransaction(); try { ids.forEach { db.delete("notices", "id=?", arrayOf(it)) }; db.setTransactionSuccessful() } finally { db.endTransaction() } }
    @Synchronized fun clearHistory() { writableDatabase.delete("messages", null, null) }
    @Synchronized fun resetAccount() {
        val db = writableDatabase; db.beginTransaction()
        try { listOf("notices", "messages", "drafts", "channels").forEach { db.delete(it, null, null) }; db.execSQL("UPDATE meta SET value=0 WHERE key='cursor'"); db.setTransactionSuccessful() } finally { db.endTransaction() }
    }
    @Synchronized fun draft(id: Long): Draft? = readableDatabase.rawQuery("SELECT text,client_id,mentions FROM drafts WHERE channel_id=?", arrayOf(id.toString())).use { if (it.moveToFirst()) Draft(id, it.getString(0), it.getString(1),it.getString(2)) else null }
    @Synchronized fun saveDraft(id: Long, text: String, mentions: String? = null): Draft {
        require(text.toByteArray().size <= 4096)
        val old = draft(id); val selected = mentions ?: adjustMentions(old?.text ?: "",text,old?.mentions ?: "[]")
        val d = Draft(id, text, if (old?.text == text && old.mentions == selected) old.clientId else newClientId(),selected)
        writableDatabase.execSQL("INSERT OR REPLACE INTO drafts VALUES(?,?,?,?)", arrayOf<Any>(id, text, d.clientId,d.mentions))
        return d
    }
    @Synchronized fun updateUsers(users: JSONArray) {
        val db=writableDatabase; db.beginTransaction()
        try { for(i in 0 until users.length()) { val u=users.getJSONObject(i); db.execSQL("UPDATE messages SET sender_name=?,sender_color=? WHERE sender_id=?",arrayOf(u.getString("name"),u.optString("color","#808080"),u.getLong("id"))) }; db.setTransactionSuccessful() } finally { db.endTransaction() }
    }
    @Synchronized fun sentDraft(d: Draft) { writableDatabase.delete("drafts", "channel_id=? AND client_id=?", arrayOf(d.channelId.toString(), d.clientId)) }
}
