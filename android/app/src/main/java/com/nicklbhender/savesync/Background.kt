package com.nicklbhender.savesync

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.util.Log
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.ExistingWorkPolicy
import androidx.work.ForegroundInfo
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.OutOfQuotaPolicy
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.unifiedpush.android.connector.FailedReason
import org.unifiedpush.android.connector.PushService
import org.unifiedpush.android.connector.data.PushEndpoint
import org.unifiedpush.android.connector.data.PushMessage

/** Starts syncing after a reboot or an app update. */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        SyncService.start(context)
    }
}

/**
 * A one-off or periodic full sync. The safety net for when the service isn't
 * running (killed by the system, or not allowed to start from the background).
 */
class SyncWorker(context: Context, params: WorkerParameters) : CoroutineWorker(context, params) {
    override suspend fun doWork(): Result = withContext(Dispatchers.IO) {
        try {
            SaveSyncApp.instance.engine.syncNow()
            Updater.check(applicationContext)
            Result.success()
        } catch (e: Exception) {
            Log.w(SaveSyncApp.TAG, "background sync failed", e)
            Result.retry()
        }
    }

    // Needed for expedited work on Android 11 and older.
    override suspend fun getForegroundInfo(): ForegroundInfo {
        val type = if (Build.VERSION.SDK_INT >= 34) ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE else 0
        return ForegroundInfo(Notifications.WORKER_ID, Notifications.service(applicationContext), type)
    }

    companion object {
        private val online = Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build()

        fun schedulePeriodic(context: Context) {
            val request = PeriodicWorkRequestBuilder<SyncWorker>(15, TimeUnit.MINUTES).setConstraints(online).build()
            WorkManager.getInstance(context)
                .enqueueUniquePeriodicWork("periodic-sync", ExistingPeriodicWorkPolicy.KEEP, request)
        }

        fun runNow(context: Context) {
            val request = OneTimeWorkRequestBuilder<SyncWorker>()
                .setConstraints(online)
                .setExpedited(OutOfQuotaPolicy.RUN_AS_NON_EXPEDITED_WORK_REQUEST)
                .build()
            WorkManager.getInstance(context).enqueueUniqueWork("sync-now", ExistingWorkPolicy.REPLACE, request)
        }
    }
}

/**
 * Receives UnifiedPush messages from the distributor (the ntfy app, pointed at
 * the NAS). A message just means "something changed": the engine fetches it.
 */
class PushServiceImpl : PushService() {
    private val app get() = SaveSyncApp.instance

    override fun onNewEndpoint(endpoint: PushEndpoint, instance: String) {
        Push.saveEndpoint(this, endpoint.url)
        app.scope.launch { Push.sendEndpoint(app) }
    }

    override fun onMessage(message: PushMessage, instance: String) {
        app.engine.handlePush(String(message.content))
        if (!SyncService.running) SyncWorker.runNow(this)
    }

    override fun onRegistrationFailed(reason: FailedReason, instance: String) {
        Log.w(SaveSyncApp.TAG, "push registration failed: $reason")
    }

    override fun onUnregistered(instance: String) {
        Push.saveEndpoint(this, null)
        app.scope.launch { Push.sendEndpoint(app) }
    }
}

object Push {
    private const val PREFS = "push"
    private const val KEY = "endpoint"

    fun saveEndpoint(context: Context, url: String?) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().putString(KEY, url).apply()
    }

    fun endpoint(context: Context): String? =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(KEY, null)

    /** Tells the server where to push. Safe to call before pairing; it's retried after. */
    suspend fun sendEndpoint(app: SaveSyncApp) = withContext(Dispatchers.IO) {
        try {
            app.engine.setPushEndpoint(endpoint(app))
        } catch (e: Exception) {
            Log.i(SaveSyncApp.TAG, "push endpoint not sent yet: ${e.message}")
        }
    }
}
