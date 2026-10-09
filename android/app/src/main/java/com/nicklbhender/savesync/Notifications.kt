package com.nicklbhender.savesync

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat

object Notifications {
    const val SERVICE_ID = 1
    const val WORKER_ID = 2
    private const val CHANNEL_SERVICE = "service"
    private const val CHANNEL_SAVES = "saves"

    fun createChannels(context: Context) {
        val manager = context.getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL_SERVICE, "Background sync", NotificationManager.IMPORTANCE_MIN).apply {
                description = "Shown while SaveSync keeps your saves in sync"
                setShowBadge(false)
            }
        )
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL_SAVES, "New saves and conflicts", NotificationManager.IMPORTANCE_DEFAULT).apply {
                description = "When another device has a newer save, or both changed it"
            }
        )
    }

    private fun openApp(context: Context): PendingIntent = PendingIntent.getActivity(
        context, 0,
        Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    fun service(context: Context): Notification =
        NotificationCompat.Builder(context, CHANNEL_SERVICE)
            .setSmallIcon(R.drawable.ic_stat_sync)
            .setContentTitle("Keeping your saves in sync")
            .setContentIntent(openApp(context))
            .setOngoing(true)
            .setSilent(true)
            .setPriority(NotificationCompat.PRIORITY_MIN)
            .build()

    private fun post(context: Context, gameId: String, title: String, text: String) {
        val notification = NotificationCompat.Builder(context, CHANNEL_SAVES)
            .setSmallIcon(R.drawable.ic_stat_sync)
            .setContentTitle(title)
            .setContentText(text)
            .setStyle(NotificationCompat.BigTextStyle().bigText(text))
            .setContentIntent(openApp(context))
            .setAutoCancel(true)
            .build()
        try {
            // One notification per game, replaced as its state changes.
            NotificationManagerCompat.from(context).notify(gameId.hashCode(), notification)
        } catch (e: SecurityException) {
            // Notifications not allowed.
        }
    }

    const val UPDATE_ID = 3
    const val ACTION_INSTALL_UPDATE = "com.nicklbhender.savesync.INSTALL_UPDATE"

    fun updateReady(context: Context, version: String) {
        val open = PendingIntent.getActivity(
            context, 1,
            Intent(context, MainActivity::class.java).setAction(ACTION_INSTALL_UPDATE).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val notification = NotificationCompat.Builder(context, CHANNEL_SAVES)
            .setSmallIcon(R.drawable.ic_stat_sync)
            .setContentTitle("SaveSync $version is ready")
            .setContentText("Tap to install the update.")
            .setContentIntent(open)
            .setAutoCancel(true)
            .build()
        try {
            NotificationManagerCompat.from(context).notify(UPDATE_ID, notification)
        } catch (e: SecurityException) {
            // Notifications not allowed; the update still shows in Settings.
        }
    }

    fun forEvent(context: Context, event: EngineEvent, snapshot: Snapshot?) {
        val gameId = event.gameId ?: return
        val name = event.gameName ?: snapshot?.games?.firstOrNull { it.config.id == gameId }?.config?.name ?: gameId
        when (event.type) {
            "import_available" -> post(
                context, gameId, "New save for $name",
                "From ${event.remote?.from ?: "another device"}. Open SaveSync to import it.",
            )
            "conflict" -> post(
                context, gameId, "Save conflict: $name",
                "This device and ${event.conflict?.remote?.from ?: "another device"} both changed it. Open SaveSync to choose which to keep.",
            )
            "imported" -> post(context, gameId, "$name updated", "Imported the latest save from your other device.")
        }
    }
}
