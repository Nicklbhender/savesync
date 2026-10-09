package com.nicklbhender.savesync

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import android.graphics.Color
import androidx.activity.SystemBarStyle
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.LaunchedEffect
import com.nicklbhender.savesync.ui.ThemePrefs
import com.nicklbhender.savesync.ui.isDarkTheme
import androidx.lifecycle.lifecycleScope
import com.nicklbhender.savesync.ui.AppRoot
import com.nicklbhender.savesync.ui.SaveSyncTheme
import kotlinx.coroutines.launch

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        ThemePrefs.load(this)
        enableEdgeToEdge()
        SyncService.start(this)
        setContent {
            // Status/navigation bar icons follow the app's theme, not just the system's.
            val dark = isDarkTheme()
            LaunchedEffect(dark) {
                val style = if (dark) SystemBarStyle.dark(Color.TRANSPARENT)
                else SystemBarStyle.light(Color.TRANSPARENT, Color.TRANSPARENT)
                enableEdgeToEdge(statusBarStyle = style, navigationBarStyle = style)
            }
            SaveSyncTheme {
                AppRoot(SaveSyncApp.instance, this)
            }
        }
        handleIntent(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleIntent(intent)
    }

    private fun handleIntent(intent: Intent?) {
        if (intent?.action == Notifications.ACTION_INSTALL_UPDATE) Updater.install(this)
    }

    override fun onResume() {
        super.onResume()
        // Opening the app is a good moment to check for new saves.
        val app = SaveSyncApp.instance
        app.engine.nudge()
        lifecycleScope.launch { app.refresh() }
        lifecycleScope.launch { Updater.check(this@MainActivity) }
    }
}
