package org.poknite

import android.content.ContentValues
import android.content.Context
import android.database.sqlite.SQLiteDatabase
import android.database.sqlite.SQLiteOpenHelper
import org.json.JSONArray

class LocalStore(context: Context, name: String = "history.db") : SQLiteOpenHelper(context, name, null, 1) {
    override fun onConfigure(db: SQLiteDatabase) {
        db.setForeignKeyConstraintsEnabled(true)
        db.rawQuery("PRAGMA journal_mode=DELETE", null).use { check(it.moveToFirst()) }
        db.rawQuery("PRAGMA max_page_count=2048", null).use { check(it.moveToFirst() && it.getLong(0) <= 2048) }
        db.execSQL("PRAGMA synchronous=FULL")
    }
    override fun onCreate(db: SQLiteDatabase) {
        db.execSQL("CREATE TABLE meta(key TEXT PRIMARY KEY,value INTEGER NOT NULL)")
        db.execSQL("INSERT INTO meta VALUES('cursor',0)")
        db.execSQL("CREATE TABLE channels(id INTEGER PRIMARY KEY,name TEXT NOT NULL,quiet INTEGER NOT NULL DEFAULT 0)")
        db.execSQL("CREATE TABLE messages(id TEXT PRIMARY KEY,seq INTEGER NOT NULL,channel_id INTEGER NOT NULL,sender_id INTEGER NOT NULL,sender_name TEXT NOT NULL,text TEXT NOT NULL,created_at INTEGER NOT NULL,expires_at INTEGER NOT NULL)")
        db.execSQL("CREATE INDEX history ON messages(channel_id,seq DESC)")
        db.execSQL("CREATE INDEX ordering ON messages(seq)")
        db.execSQL("CREATE TABLE notices(id TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,replay INTEGER NOT NULL)")
        db.execSQL("CREATE TABLE drafts(channel_id INTEGER PRIMARY KEY,text TEXT NOT NULL,client_id TEXT NOT NULL)")
    }
    override fun onUpgrade(db: SQLiteDatabase, oldVersion: Int, newVersion: Int) = Unit
    @Synchronized fun cursor(): Long = readableDatabase.rawQuery("SELECT value FROM meta WHERE key='cursor'", null).use { it.moveToFirst(); it.getLong(0) }
    @Synchronized fun progress(cursor: Long, reset: Boolean = false) {
        require(cursor >= 0)
        writableDatabase.execSQL(if (reset) "UPDATE meta SET value=? WHERE key='cursor'" else "UPDATE meta SET value=MAX(value,?) WHERE key='cursor'", arrayOf(cursor))
    }
    // History and pending notification are committed atomically before an ACK is sent.
    @Synchronized fun receive(m: WireMessage, replay: Boolean, ownUser: Long, advanceCursor: Boolean = true): Boolean {
        val db = writableDatabase
        var inserted = false
        db.beginTransaction()
        try {
            val exists = db.rawQuery("SELECT 1 FROM messages WHERE id=?", arrayOf(m.id)).use { it.moveToFirst() }
            if (!exists) {
                db.execSQL("DELETE FROM messages WHERE id IN (SELECT id FROM messages ORDER BY seq DESC LIMIT -1 OFFSET 999)")
                val v = ContentValues().apply { put("id", m.id); put("seq", m.seq); put("channel_id", m.channelId); put("sender_id", m.senderId); put("sender_name", m.senderName); put("text", m.text); put("created_at", m.createdAt); put("expires_at", m.expiresAt) }
                db.insertOrThrow("messages", null, v)
                if (m.senderId != ownUser) db.execSQL("INSERT INTO notices VALUES(?,?)", arrayOf<Any>(m.id, if (replay) 1 else 0))
                inserted = true
            }
            // Seq is safe only for this message; Progress will also cover invisible channels.
            if (advanceCursor) db.execSQL("UPDATE meta SET value=MAX(value,?) WHERE key='cursor'", arrayOf(m.seq))
            db.setTransactionSuccessful()
        } finally { db.endTransaction() }
        return inserted
    }
    @Synchronized fun updateChannels(array: JSONArray) {
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
                db.execSQL("UPDATE channels SET name=? WHERE id=?", arrayOf(name, id))
            }
            if (ids.isEmpty()) db.delete("channels", null, null)
            else db.execSQL("DELETE FROM channels WHERE id NOT IN (${ids.joinToString(",")})")
            db.setTransactionSuccessful()
        } finally { db.endTransaction() }
    }
    @Synchronized fun channels(): List<Channel> = readableDatabase.rawQuery("SELECT id,name FROM channels ORDER BY id", null).use { c -> buildList { while (c.moveToNext()) add(Channel(c.getLong(0), c.getString(1))) } }
    @Synchronized fun quiet(id: Long): Boolean = readableDatabase.rawQuery("SELECT quiet FROM channels WHERE id=?", arrayOf(id.toString())).use { it.moveToFirst() && it.getInt(0) != 0 }
    @Synchronized fun setQuiet(id: Long, quiet: Boolean) { writableDatabase.execSQL("UPDATE channels SET quiet=? WHERE id=?", arrayOf(if (quiet) 1 else 0, id)) }
    @Synchronized fun history(channel: Long, offset: Int = 0): List<WireMessage> = readableDatabase.rawQuery("SELECT id,seq,channel_id,sender_id,sender_name,text,created_at,expires_at FROM messages WHERE channel_id=? ORDER BY seq DESC LIMIT 50 OFFSET ?", arrayOf(channel.toString(), offset.coerceIn(0, 1000).toString())).use { c -> buildList { while (c.moveToNext()) add(WireMessage(c.getString(0), c.getLong(1), c.getLong(2), c.getLong(3), c.getString(4), c.getString(5), c.getLong(6), c.getLong(7))) } }
    @Synchronized fun pending(replay: Boolean): List<WireMessage> = readableDatabase.rawQuery("SELECT m.id,m.seq,m.channel_id,m.sender_id,m.sender_name,m.text,m.created_at,m.expires_at FROM notices n JOIN messages m ON m.id=n.id WHERE n.replay=? ORDER BY m.seq LIMIT 1000", arrayOf(if (replay) "1" else "0")).use { c -> buildList { while (c.moveToNext()) add(WireMessage(c.getString(0), c.getLong(1), c.getLong(2), c.getLong(3), c.getString(4), c.getString(5), c.getLong(6), c.getLong(7))) } }
    @Synchronized fun shown(ids: List<String>) { val db = writableDatabase; db.beginTransaction(); try { ids.forEach { db.delete("notices", "id=?", arrayOf(it)) }; db.setTransactionSuccessful() } finally { db.endTransaction() } }
    @Synchronized fun clearHistory() { writableDatabase.delete("messages", null, null) }
    @Synchronized fun resetAccount() {
        val db = writableDatabase; db.beginTransaction()
        try { listOf("notices", "messages", "drafts", "channels").forEach { db.delete(it, null, null) }; db.execSQL("UPDATE meta SET value=0 WHERE key='cursor'"); db.setTransactionSuccessful() } finally { db.endTransaction() }
    }
    @Synchronized fun draft(id: Long): Draft? = readableDatabase.rawQuery("SELECT text,client_id FROM drafts WHERE channel_id=?", arrayOf(id.toString())).use { if (it.moveToFirst()) Draft(id, it.getString(0), it.getString(1)) else null }
    @Synchronized fun saveDraft(id: Long, text: String): Draft {
        require(text.toByteArray().size <= 4096)
        val old = draft(id); val d = Draft(id, text, if (old?.text == text) old.clientId else newClientId())
        writableDatabase.execSQL("INSERT OR REPLACE INTO drafts VALUES(?,?,?)", arrayOf<Any>(id, text, d.clientId))
        return d
    }
    @Synchronized fun sentDraft(d: Draft) { writableDatabase.delete("drafts", "channel_id=? AND client_id=?", arrayOf(d.channelId.toString(), d.clientId)) }
}
