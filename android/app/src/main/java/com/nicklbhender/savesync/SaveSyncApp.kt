package com.nicklbhender.savesync

import android.app.Application
import android.os.Build
import android.provider.Settings
import android.util.Log
import java.io.File
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.savesync_ffi.EventListener
import uniffi.savesync_ffi.SaveSync

/**
 * Owns the sync engine for the whole process, independent of any screen, so it
 * keeps working for the foreground service, background workers and push messages.
 */
class SaveSyncApp : Application() {
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    private val _state = MutableStateFlow<Snapshot?>(null)
    val state: StateFlow<Snapshot?> = _state

    private val _events = MutableSharedFlow<EngineEvent>(extraBufferCapacity = 64)
    val events: SharedFlow<EngineEvent> = _events

    val engine: SaveSync by lazy {
        SaveSync(File(filesDir, "engine").path, defaultDeviceName(), SafFolderAccess(this)).also {
            it.setListener(object : EventListener {
                // Called on an engine thread: hand off immediately.
                override fun onEvent(eventJson: String) {
                    scope.launch { handleEvent(eventJson) }
                }
            })
        }
    }

    override fun onCreate() {
        super.onCreate()
        instance = this
        Notifications.createChannels(this)
        SyncWorker.schedulePeriodic(this)
    }

    /** Reloads the engine's state for the UI. */
    suspend fun refresh(): Snapshot? = withContext(Dispatchers.IO) {
        try {
            json.decodeFromString<Snapshot>(engine.snapshot()).also { _state.value = it }
        } catch (e: Exception) {
            Log.w(TAG, "snapshot failed", e)
            null
        }
    }

    private suspend fun handleEvent(eventJson: String) {
        val event = try {
            json.decodeFromString<EngineEvent>(eventJson)
        } catch (e: Exception) {
            Log.w(TAG, "unrecognized event: $eventJson", e)
            return
        }
        refresh()
        _events.emit(event)
        Notifications.forEvent(this, event, _state.value)
    }

    fun defaultDeviceName(): String =
        Settings.Global.getString(contentResolver, Settings.Global.DEVICE_NAME)?.takeIf { it.isNotBlank() }
            ?: "${Build.MANUFACTURER.replaceFirstChar { it.uppercase() }} ${Build.MODEL}"

    companion object {
        const val TAG = "SaveSync"
        lateinit var instance: SaveSyncApp
            private set
    }
}
