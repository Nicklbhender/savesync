package com.nicklbhender.savesync.ui

import android.app.Activity
import android.text.format.DateUtils
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.nicklbhender.savesync.GameView
import com.nicklbhender.savesync.SaveSyncApp
import com.nicklbhender.savesync.SessionWatcher
import com.nicklbhender.savesync.Snapshot
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.savesync_ffi.FfiException

sealed interface Screen {
    data object Games : Screen
    data object Settings : Screen
    data class Edit(val game: GameView?) : Screen
}

fun errorText(e: Throwable): String = when (e) {
    is FfiException.Failed -> e.detail
    is FfiException.NotFound -> e.detail
    else -> e.message ?: "Something went wrong"
}

fun ago(ms: Long?): String =
    if (ms == null || ms <= 0) "at an unknown time"
    else if (System.currentTimeMillis() - ms < 45_000) "just now"
    else DateUtils.getRelativeTimeSpanString(ms, System.currentTimeMillis(), DateUtils.MINUTE_IN_MILLIS).toString().lowercase()

/** Runs engine calls off the main thread, reporting failures in a snackbar. */
class Actions(private val app: SaveSyncApp, private val scope: CoroutineScope, val snackbar: SnackbarHostState) {
    val busy = mutableStateListOf<String>()

    fun run(key: String, success: String? = null, onDone: () -> Unit = {}, block: suspend () -> Unit) {
        if (key in busy) return
        busy += key
        scope.launch {
            try {
                withContext(Dispatchers.IO) { block() }
                onDone()
                success?.let { scope.launch { snackbar.showSnackbar(it) } }
            } catch (e: Exception) {
                scope.launch { snackbar.showSnackbar(errorText(e)) }
            } finally {
                busy -= key
                app.refresh()
            }
        }
    }
}

@Composable
fun AppRoot(app: SaveSyncApp, activity: Activity) {
    val snapshot by app.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    val snackbar = remember { SnackbarHostState() }
    val actions = remember { Actions(app, scope, snackbar) }
    var screen by remember { mutableStateOf<Screen>(Screen.Games) }

    LaunchedEffect(Unit) {
        // Keeps "played 5 minutes ago" and the online state current.
        while (true) {
            app.refresh()
            delay(20_000)
        }
    }
    LaunchedEffect(Unit) {
        app.events.collect { e ->
            if (e.type == "uploaded" && e.created == true) {
                val name = app.state.value?.games?.firstOrNull { it.config.id == e.gameId }?.config?.name ?: e.gameId
                snackbar.showSnackbar("Uploaded your progress in $name")
            }
        }
    }
    BackHandler(enabled = screen != Screen.Games) { screen = Screen.Games }

    val s = snapshot
    when {
        s == null -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        s.server == null -> SetupScreen(app, activity, actions)
        else -> when (val current = screen) {
            Screen.Games -> GamesScreen(s, app, actions, onAdd = { screen = Screen.Edit(null) },
                onEdit = { screen = Screen.Edit(it) }, onSettings = { screen = Screen.Settings })
            Screen.Settings -> SettingsScreen(s, app, activity, actions, onBack = { screen = Screen.Games })
            is Screen.Edit -> EditGameScreen(current.game, app, actions, onDone = { screen = Screen.Games })
        }
    }
}

// ---------------------------------------------------------------- games list

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun GamesScreen(
    s: Snapshot,
    app: SaveSyncApp,
    actions: Actions,
    onAdd: () -> Unit,
    onEdit: (GameView) -> Unit,
    onSettings: () -> Unit,
) {
    val context = LocalContext.current
    var backupsFor by remember { mutableStateOf<GameView?>(null) }
    var confirmKeepMine by remember { mutableStateOf<GameView?>(null) }
    val needsUsageAccess = s.games.any { it.config.processes.isNotEmpty() } && !SessionWatcher.hasUsageAccess(context)

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text("SaveSync", fontWeight = FontWeight.SemiBold)
                        Spacer(Modifier.width(10.dp))
                        Badge(if (s.online) "Online" else "Offline", if (s.online) "synced" else "offline")
                    }
                },
                actions = {
                    TextButton(onClick = { actions.run("sync") { app.engine.syncNow() } }, enabled = "sync" !in actions.busy) {
                        Text(if ("sync" in actions.busy) "Syncing…" else "Sync")
                    }
                    TextButton(onClick = onSettings) { Text("Settings") }
                },
            )
        },
        floatingActionButton = { ExtendedFloatingActionButton(onClick = onAdd) { Text("Add save") } },
        snackbarHost = { SnackbarHost(actions.snackbar) },
    ) { padding ->
        LazyColumn(
            contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = padding.calculateTopPadding() + 4.dp, bottom = padding.calculateBottomPadding() + 88.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
            modifier = Modifier.fillMaxSize(),
        ) {
            if (!s.online) item {
                Banner("offline", "Can't reach ${s.server?.url} right now. Saves are still captured on this device and upload once it's reachable.")
            }
            if (needsUsageAccess) item {
                Banner("queued", "Allow usage access so SaveSync can tell when you're playing. Without it, saves only sync when you open SaveSync or every 15 minutes.") {
                    TextButton(onClick = onSettings) { Text("Set up") }
                }
            }
            if (s.games.isEmpty()) item { EmptyState(onAdd) }
            items(s.games, key = { it.config.id }) { game ->
                GameCard(game, s.online, actions, app,
                    onEdit = { onEdit(game) }, onBackups = { backupsFor = game }, onKeepMine = { confirmKeepMine = game })
            }
        }
    }

    backupsFor?.let { BackupsDialog(it, app, actions, onDismiss = { backupsFor = null }) }
    confirmKeepMine?.let { game ->
        ConfirmDialog(
            title = "Keep this device's save?",
            text = "It becomes the newest version of ${game.config.name} and replaces the save from ${game.staged?.from} on your other devices.",
            confirm = "Keep mine",
            onConfirm = { actions.run(game.config.id) { app.engine.keepLocal(game.config.id) } },
            onDismiss = { confirmKeepMine = null },
        )
    }
}

@Composable
fun Badge(text: String, kind: String) {
    val c = StatusColors.of(kind, isDarkTheme())
    Text(
        text, color = c.fg, style = MaterialTheme.typography.labelMedium, fontWeight = FontWeight.SemiBold,
        modifier = Modifier.clip(RoundedCornerShape(6.dp)).background(c.bg).padding(horizontal = 8.dp, vertical = 3.dp),
    )
}

@Composable
fun Banner(kind: String, text: String, action: (@Composable () -> Unit)? = null) {
    val c = StatusColors.of(kind, isDarkTheme())
    Row(
        Modifier.fillMaxWidth().clip(RoundedCornerShape(12.dp)).background(c.bg).padding(start = 14.dp, end = 6.dp, top = 10.dp, bottom = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(text, color = c.fg, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
        action?.invoke()
    }
}

@Composable
private fun EmptyState(onAdd: () -> Unit) {
    Column(Modifier.fillMaxWidth().padding(vertical = 56.dp, horizontal = 24.dp), horizontalAlignment = Alignment.CenterHorizontally) {
        Text("No games yet", style = MaterialTheme.typography.titleLarge)
        Spacer(Modifier.height(8.dp))
        Text(
            "Add a save and choose its folder. Use the same Save ID as on your other devices so they sync together.",
            style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(16.dp))
        Button(onClick = onAdd) { Text("Add your first save") }
    }
}

private data class Status(val kind: String, val label: String, val detail: String)

private fun status(g: GameView, online: Boolean): Status = when {
    g.conflict != null -> Status(
        "conflict", "Conflict",
        "This device${g.conflict.localPlayedAt?.let { " (played ${ago(it)})" } ?: ""} and ${g.conflict.remote.from} " +
            "(played ${ago(g.conflict.remote.playedOrCreated)}) both changed this save.",
    )
    g.inSession -> Status(
        "playing", "Playing",
        g.staged?.let { "A newer save from ${it.from} is waiting until you close the game." }
            ?: "Your progress will upload when you leave the game.",
    )
    g.staged != null -> Status("import", "New save", "From ${g.staged.from}, played ${ago(g.staged.playedOrCreated)}.")
    g.lastError != null -> Status("error", "Needs attention", g.lastError)
    g.queuedUpload != null -> {
        val played = ago(g.queuedUpload.playedAt ?: g.queuedUpload.createdAt)
        if (online) Status("queued", "Uploading", "Played $played.")
        else Status("queued", "Waiting to upload", "Played $played. It will upload once the server is reachable.")
    }
    g.baseVersion == 0L -> Status("idle", "No saves yet", "Nothing synced yet. Play it here or on another device.")
    else -> Status("synced", "Synced", "Up to date (version ${g.baseVersion}).")
}

@Composable
private fun GameCard(
    g: GameView,
    online: Boolean,
    actions: Actions,
    app: SaveSyncApp,
    onEdit: () -> Unit,
    onBackups: () -> Unit,
    onKeepMine: () -> Unit,
) {
    val st = status(g, online)
    val id = g.config.id
    val busy = id in actions.busy
    Card(
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainerLow),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Row(verticalAlignment = Alignment.Top) {
                Text(g.config.name, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
                Spacer(Modifier.width(8.dp))
                Badge(st.label, st.kind)
            }
            Text(st.detail, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)

            if (g.conflict != null) {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton(onClick = { actions.run(id, "Kept this device's save") { app.engine.resolveConflict(id, "keep_local") } },
                        enabled = !busy, modifier = Modifier.weight(1f)) { Text("Keep mine") }
                    Button(onClick = { actions.run(id, "Using the save from ${g.conflict.remote.from}") { app.engine.resolveConflict(id, "keep_remote") } },
                        enabled = !busy, modifier = Modifier.weight(1f)) { Text("Use theirs") }
                }
                Text("Nothing is lost: replaced saves are kept as backups and in the server's history.",
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            } else if (g.staged != null && !g.inSession) {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton(onClick = onKeepMine, enabled = !busy, modifier = Modifier.weight(1f)) { Text("Keep mine") }
                    Button(onClick = { actions.run(id, "Imported the new save for ${g.config.name}") { app.engine.importSave(id) } },
                        enabled = !busy, modifier = Modifier.weight(1f)) { Text("Import") }
                }
            }

            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(folderLabel(g.config.location), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
                TextButton(onClick = onBackups) { Text("Backups") }
                TextButton(onClick = onEdit) { Text("Edit") }
            }
        }
    }
}

/** A readable label like "Games/Saves" for a SAF tree URI on the internal storage. */
fun folderLabel(location: String): String = try {
    val id = android.provider.DocumentsContract.getTreeDocumentId(android.net.Uri.parse(location))
    id.substringAfter(':').ifEmpty { "Internal storage" }.let { if (id.startsWith("primary:")) it else "${id.substringBefore(':')} · $it" }
} catch (e: Exception) {
    location
}
