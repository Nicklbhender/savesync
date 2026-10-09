package com.nicklbhender.savesync.ui

import android.content.Context
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.graphics.Color
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

// Same palette as the desktop app and icon: indigo with a teal accent.
private val Light = lightColorScheme(
    primary = Color(0xFF3F51D9),
    onPrimary = Color.White,
    primaryContainer = Color(0xFFE0E4FF),
    onPrimaryContainer = Color(0xFF0E1A6E),
    secondary = Color(0xFF0F8F82),
    background = Color(0xFFF6F7F9),
    surface = Color(0xFFF6F7F9),
    surfaceContainerLow = Color.White,
    error = Color(0xFFC4312B),
)

// Neutral grays (no blue tint) with the indigo accent.
private val Dark = darkColorScheme(
    primary = Color(0xFF9FA8FF),
    onPrimary = Color(0xFF101010),
    primaryContainer = Color(0xFF34396B),
    onPrimaryContainer = Color(0xFFE0E4FF),
    secondary = Color(0xFF4FD1C2),
    background = Color(0xFF121212),
    surface = Color(0xFF121212),
    surfaceContainer = Color(0xFF1E1E1E),
    surfaceContainerLow = Color(0xFF1C1C1C),
    surfaceContainerHigh = Color(0xFF262626),
    surfaceContainerHighest = Color(0xFF2E2E2E),
    onSurface = Color(0xFFE6E6E6),
    onSurfaceVariant = Color(0xFFA6A6A6),
    outline = Color(0xFF5A5A5A),
    outlineVariant = Color(0xFF3A3A3A),
    error = Color(0xFFFF7A73),
)

/** The user's appearance choice: "system", "light" or "dark". */
object ThemePrefs {
    private const val PREFS = "appearance"
    private val _mode = MutableStateFlow("system")
    val mode: StateFlow<String> = _mode

    fun load(context: Context) {
        _mode.value = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString("theme", "system") ?: "system"
    }

    fun set(context: Context, mode: String) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().putString("theme", mode).apply()
        _mode.value = mode
    }
}

@Composable
fun isDarkTheme(): Boolean {
    val mode by ThemePrefs.mode.collectAsStateWithLifecycle()
    return when (mode) {
        "light" -> false
        "dark" -> true
        else -> isSystemInDarkTheme()
    }
}

object StatusColors {
    data class Pair(val fg: Color, val bg: Color)

    fun of(kind: String, dark: Boolean): Pair = when (kind) {
        "synced" -> if (dark) Pair(Color(0xFF4CCF8C), Color(0xFF1B2E22)) else Pair(Color(0xFF138A52), Color(0xFFE5F6ED))
        "import" -> if (dark) Pair(Color(0xFF9FA8FF), Color(0xFF262A40)) else Pair(Color(0xFF3F51D9), Color(0xFFEAEDFD))
        "conflict", "error" -> if (dark) Pair(Color(0xFFFF7A73), Color(0xFF3A1E1E)) else Pair(Color(0xFFC4312B), Color(0xFFFDEAEA))
        "queued", "playing", "offline" -> if (dark) Pair(Color(0xFFF2B84B), Color(0xFF332A18)) else Pair(Color(0xFFA15C00), Color(0xFFFDF1DC))
        else -> if (dark) Pair(Color(0xFFA6A6A6), Color(0xFF2A2A2A)) else Pair(Color(0xFF5D6573), Color(0xFFEEF0F3))
    }
}

@Composable
fun SaveSyncTheme(content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = if (isDarkTheme()) Dark else Light, content = content)
}
