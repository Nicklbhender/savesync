package com.nicklbhender.savesync

import android.app.AppOpsManager
import android.app.usage.UsageEvents
import android.app.usage.UsageStatsManager
import android.content.Context
import android.os.PowerManager
import android.os.Process
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive

/**
 * Watches which app is in the foreground (usage access), and turns an emulator
 * coming to / leaving the foreground into a game session for the engine. Works
 * with any game launcher or frontend, since it doesn't need to launch the game.
 *
 * Any other app coming to the foreground (e.g. opening the frontend) also
 * triggers a quick check for new saves.
 */
class SessionWatcher(private val context: Context, private val app: SaveSyncApp) {
    private val usage = context.getSystemService(UsageStatsManager::class.java)
    private val power = context.getSystemService(PowerManager::class.java)
    private var foreground: String? = null
    private var activeGames = emptySet<String>()
    private var lastNudge = 0L

    suspend fun run() {
        var since = System.currentTimeMillis() - 5_000
        while (currentCoroutineContext().isActive) {
            if (hasUsageAccess(context) && (power.isInteractive || activeGames.isNotEmpty())) {
                val now = System.currentTimeMillis()
                val previous = foreground
                readEvents(since, now)
                since = now
                update(previous)
            }
            delay(2_000)
        }
    }

    private fun readEvents(from: Long, to: Long) {
        val events = usage.queryEvents(from, to) ?: return
        val event = UsageEvents.Event()
        while (events.hasNextEvent()) {
            events.getNextEvent(event)
            when (event.eventType) {
                UsageEvents.Event.ACTIVITY_RESUMED -> foreground = event.packageName
                // Also fires when the screen turns off, which ends the session so the
                // save (written when the emulator pauses) gets uploaded.
                UsageEvents.Event.ACTIVITY_PAUSED -> if (event.packageName == foreground) foreground = null
            }
        }
    }

    private fun update(previous: String?) {
        val games = app.state.value?.games ?: return
        val current = foreground
        val nowActive = if (current == null) emptySet() else games
            .filter { g -> g.config.processes.any { it.equals(current, ignoreCase = true) } }
            .map { it.config.id }
            .toSet()
        (nowActive - activeGames).forEach { app.engine.sessionStarted(it) }
        (activeGames - nowActive).forEach { app.engine.sessionEnded(it) }
        activeGames = nowActive

        val now = System.currentTimeMillis()
        if (current != previous && current != null && nowActive.isEmpty() && now - lastNudge > 30_000) {
            lastNudge = now
            app.engine.nudge()
        }
    }

    companion object {
        fun hasUsageAccess(context: Context): Boolean {
            val ops = context.getSystemService(AppOpsManager::class.java)
            val mode = ops.unsafeCheckOpNoThrow(AppOpsManager.OPSTR_GET_USAGE_STATS, Process.myUid(), context.packageName)
            return mode == AppOpsManager.MODE_ALLOWED
        }
    }
}
