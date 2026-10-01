package org.poknite

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action !in setOf(Intent.ACTION_BOOT_COMPLETED, Intent.ACTION_MY_PACKAGE_REPLACED)) return
        val app = context.applicationContext as PokniteApp
        if (!app.settings.enabled || app.settings.token.isEmpty()) return
        try { context.startForegroundService(Intent(context, ConnectionService::class.java)) }
        catch (_: RuntimeException) { app.settings.status = "Откройте Poknite для возобновления приёма" }
    }
}
