package com.nicklbhender.savesync.ui

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.nicklbhender.savesync.BackupInfo
import com.nicklbhender.savesync.GameConfig
import com.nicklbhender.savesync.GameView
import com.nicklbhender.savesync.Push
import com.nicklbhender.savesync.SaveSyncApp
import com.nicklbhender.savesync.SessionWatcher
import com.nicklbhender.savesync.Snapshot
import com.nicklbhender.savesync.json
import java.text.Normalizer
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.unifiedpush.android.connector.UnifiedPush

// ---------------------------------------------------------------- setup

const val LOCAL_NETWORK = "android.permission.ACCESS_LOCAL_NETWORK"

/** Android 17+ asks before apps can connect to devices on the home network, like the NAS. */
fun needsLocalNetworkPermission(context: android.content.Context): Boolean =
    Build.VERSION.SDK_INT >= 37 &&
        ContextCompat.checkSelfPermission(context, LOCAL_NETWORK) != PackageManager.PERMISSION_GRANTED

@Composable
fun SetupScreen(app: SaveSyncApp, activity: Activity, actions: Actions) {
    var url by remember { mutableStateOf("") }
    var key by remember { mutableStateOf("") }
    var name by remember { mutableStateOf(app.defaultDeviceName()) }
    val connecting = "pair" in actions.busy
    val context = LocalContext.current

    fun connect() {
        actions.run("pair", "Connected to your server", onDone = { registerPush(activity, app) }) {
            val address = url.trim().let { if (it.startsWith("http")) it else "http://$it" }
            app.engine.pair(address, key.trim(), name.trim())
            Push.sendEndpoint(app)
        }
    }
    val askLocalNetwork = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { connect() }

    Scaffold(snackbarHost = { SnackbarHost(actions.snackbar) }) { padding ->
        Column(
            Modifier.fillMaxSize().padding(padding).imePadding().verticalScroll(rememberScrollState()).padding(24.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            Text("Connect to your SaveSync server", style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.SemiBold)
            Text("Saves sync through the SaveSync server on your NAS. You only need to do this once on each device.",
                color = MaterialTheme.colorScheme.onSurfaceVariant)
            OutlinedTextField(url, { url = it }, label = { Text("Server address") }, placeholder = { Text("http://192.168.1.50:8420") },
                supportingText = { Text("Your NAS's name or IP address, with port 8420.") }, singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri), modifier = Modifier.fillMaxWidth())
            OutlinedTextField(key, { key = it }, label = { Text("Enroll key") }, visualTransformation = PasswordVisualTransformation(),
                supportingText = { Text("The SAVESYNC_ENROLL_KEY from your docker-compose.yml.") }, singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password), modifier = Modifier.fillMaxWidth())
            OutlinedTextField(name, { name = it }, label = { Text("This device's name") },
                supportingText = { Text("Your other devices will show it, e.g. “New save from $name”.") }, singleLine = true,
                modifier = Modifier.fillMaxWidth())
            if (needsLocalNetworkPermission(context)) {
                Text("Android will ask to let SaveSync find and connect to nearby devices. That's how it reaches your NAS on your home network.",
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            Button(
                onClick = { if (needsLocalNetworkPermission(context)) askLocalNetwork.launch(LOCAL_NETWORK) else connect() },
                enabled = !connecting && url.isNotBlank() && key.isNotBlank() && name.isNotBlank(),
                modifier = Modifier.fillMaxWidth(),
            ) { Text(if (connecting) "Connecting…" else "Connect") }
        }
    }
}

/** Asks the user's UnifiedPush distributor (ntfy) for an endpoint, if one is installed. */
fun registerPush(activity: Activity, app: SaveSyncApp) {
    UnifiedPush.tryUseCurrentOrDefaultDistributor(activity) { ok ->
        if (ok) UnifiedPush.register(app)
    }
}

// ---------------------------------------------------------------- add / edit

private val SAVE_ID = Regex("^[a-z0-9][a-z0-9._-]{0,63}$")

private fun slugify(name: String): String =
    Normalizer.normalize(name, Normalizer.Form.NFKD).replace(Regex("\\p{M}+"), "").lowercase()
        .replace(Regex("[^a-z0-9]+"), "-").trim('-').take(64)

private fun patterns(text: String): List<String> = text.split(',', '\n').map { it.trim() }.filter { it.isNotEmpty() }

data class InstalledApp(val label: String, val packageName: String)


@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun EditGameScreen(game: GameView?, app: SaveSyncApp, actions: Actions, onDone: () -> Unit) {
    val context = LocalContext.current
    val editing = game != null
    val c = game?.config
    var name by remember { mutableStateOf(c?.name ?: "") }
    var id by remember { mutableStateOf(c?.id ?: "") }
    var idTouched by remember { mutableStateOf(editing) }
    var location by remember { mutableStateOf(c?.location ?: "") }
    var include by remember { mutableStateOf(c?.include?.joinToString(", ") ?: "") }
    var ignore by remember { mutableStateOf(c?.ignore?.joinToString(", ") ?: "") }
    var processes by remember { mutableStateOf(c?.processes ?: emptyList()) }
    var policy by remember { mutableStateOf(c?.importPolicy ?: "auto_when_safe") }
    var pickingApps by remember { mutableStateOf(false) }
    var confirmRemove by remember { mutableStateOf(false) }

    val pickFolder = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri ->
        if (uri != null) {
            // Keep access across restarts.
            context.contentResolver.takePersistableUriPermission(
                uri, Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION,
            )
            location = uri.toString()
        }
    }
    val apps = remember {
        val launcher = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
        context.packageManager.queryIntentActivities(launcher, 0)
            .map { InstalledApp(it.loadLabel(context.packageManager).toString(), it.activityInfo.packageName) }
            .filter { it.packageName != context.packageName }
            .distinctBy { it.packageName }
            .sortedBy { it.label.lowercase() }
    }
    val saving = (c?.id ?: "new") in actions.busy

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(if (editing) "Edit ${c!!.name}" else "Add a save") },
                navigationIcon = { TextButton(onClick = onDone) { Text("Cancel") } },
            )
        },
        snackbarHost = { SnackbarHost(actions.snackbar) },
    ) { padding ->
        Column(
            Modifier.fillMaxSize().padding(padding).imePadding().verticalScroll(rememberScrollState()).padding(horizontal = 20.dp, vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            OutlinedTextField(name, { name = it; if (!idTouched) id = slugify(it) }, label = { Text("Name") },
                placeholder = { Text("Game name") }, singleLine = true, modifier = Modifier.fillMaxWidth())
            val idInvalid = id.isNotEmpty() && !SAVE_ID.matches(id.trim())
            OutlinedTextField(id, { id = it; idTouched = true }, label = { Text("Save ID") }, singleLine = true, readOnly = editing,
                isError = idInvalid,
                supportingText = { Text(if (editing) "Can't be changed." else "Use the same Save ID on every device. Lowercase letters, numbers, dashes, underscores and dots only, with no spaces, e.g. my-save-1.") },
                modifier = Modifier.fillMaxWidth())

            Section("Save folder")
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(if (location.isEmpty()) "No folder chosen" else folderLabel(location), modifier = Modifier.weight(1f),
                    color = if (location.isEmpty()) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface)
                OutlinedButton(onClick = { pickFolder.launch(null) }) { Text(if (location.isEmpty()) "Choose" else "Change") }
            }
            Hint("The folder where this game's save files are stored. Folders inside Android/data can't be used.")

            OutlinedTextField(include, { include = it }, label = { Text("Only sync (optional)") },
                placeholder = { Text("*.sav") }, modifier = Modifier.fillMaxWidth(),
                supportingText = { Text("Separate multiple entries with commas.") })
            OutlinedTextField(ignore, { ignore = it }, label = { Text("Never sync (optional)") }, placeholder = { Text("*.bak") },
                modifier = Modifier.fillMaxWidth())

            Section("App/Game")
            val chosen = processes.map { p -> apps.firstOrNull { it.packageName == p }?.label ?: p }
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(if (chosen.isEmpty()) "None chosen" else chosen.joinToString(", "), modifier = Modifier.weight(1f))
                OutlinedButton(onClick = { pickingApps = true }) { Text("Choose") }
            }
            Hint("While this app/game is open, SaveSync won't replace your save with one from another device, and it uploads your progress only after you close it.")

            Section("When another device has a newer save")
            Choice(policy == "auto_when_safe", "Import automatically", "Unless this device has unsynced progress.") { policy = "auto_when_safe" }
            Choice(policy == "ask", "Ask me", "Download it and let me choose when to import.") { policy = "ask" }

            Spacer(Modifier.heightIn(min = 4.dp))
            Button(
                onClick = {
                    val config = GameConfig(id.trim(), name.trim(), location, patterns(include), patterns(ignore), processes, policy)
                    val body = json.encodeToString(GameConfig.serializer(), config)
                    actions.run(c?.id ?: "new", if (editing) "Saved" else "Added ${config.name}", onDone = onDone) {
                        if (editing) app.engine.updateGame(body) else app.engine.addGame(body)
                    }
                },
                enabled = !saving && name.isNotBlank() && SAVE_ID.matches(id.trim()) && location.isNotBlank(),
                modifier = Modifier.fillMaxWidth(),
            ) { Text(if (editing) "Save changes" else "Add save") }
            if (editing) {
                TextButton(onClick = { confirmRemove = true }, modifier = Modifier.fillMaxWidth()) {
                    Text("Stop syncing this game", color = MaterialTheme.colorScheme.error)
                }
            }
            Spacer(Modifier.heightIn(min = 24.dp))
        }
    }

    if (pickingApps) {
        AppPickerDialog(apps, processes, onDismiss = { pickingApps = false }, onDone = { processes = it; pickingApps = false })
    }
    if (confirmRemove && c != null) {
        ConfirmDialog(
            title = "Stop syncing ${c.name}?",
            text = "SaveSync stops watching this game on this device. Your save files and the server's copies aren't deleted.",
            confirm = "Stop syncing",
            onConfirm = { actions.run(c.id, "Stopped syncing ${c.name}", onDone = onDone) { app.engine.removeGame(c.id) } },
            onDismiss = { confirmRemove = false },
        )
    }
}

@Composable
private fun Section(text: String) {
    Text(text, style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold, modifier = Modifier.padding(top = 8.dp))
}

@Composable
private fun Hint(text: String) {
    Text(text, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
}

@Composable
private fun Choice(selected: Boolean, title: String, detail: String, onClick: () -> Unit) {
    Row(Modifier.fillMaxWidth().clickable(onClick = onClick).padding(vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        RadioButton(selected = selected, onClick = onClick)
        Column {
            Text(title)
            Hint(detail)
        }
    }
}

@Composable
private fun AppPickerDialog(apps: List<InstalledApp>, selected: List<String>, onDismiss: () -> Unit, onDone: (List<String>) -> Unit) {
    var chosen by remember { mutableStateOf(selected.toSet()) }
    var query by remember { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Choose the app/game") },
        text = {
            Column {
                OutlinedTextField(query, { query = it }, placeholder = { Text("Search apps") }, singleLine = true, modifier = Modifier.fillMaxWidth())
                LazyColumn(Modifier.heightIn(max = 420.dp)) {
                    items(apps.filter { query.isBlank() || it.label.contains(query, true) || it.packageName.contains(query, true) }) { a ->
                        Row(Modifier.fillMaxWidth().clickable {
                            chosen = if (a.packageName in chosen) chosen - a.packageName else chosen + a.packageName
                        }.padding(vertical = 2.dp), verticalAlignment = Alignment.CenterVertically) {
                            Checkbox(checked = a.packageName in chosen, onCheckedChange = null)
                            Column(Modifier.padding(start = 8.dp)) {
                                Text(a.label)
                                Hint(a.packageName)
                            }
                        }
                    }
                }
            }
        },
        confirmButton = { TextButton(onClick = { onDone(chosen.toList()) }) { Text("Done") } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

// ---------------------------------------------------------------- dialogs

@Composable
fun ConfirmDialog(title: String, text: String, confirm: String, onConfirm: () -> Unit, onDismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = { Text(text) },
        confirmButton = { TextButton(onClick = { onDismiss(); onConfirm() }) { Text(confirm) } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

@Composable
fun BackupsDialog(game: GameView, app: SaveSyncApp, actions: Actions, onDismiss: () -> Unit) {
    var backups by remember { mutableStateOf<List<BackupInfo>?>(null) }
    var restore by remember { mutableStateOf<BackupInfo?>(null) }
    LaunchedEffect(game.config.id) {
        backups = withContext(Dispatchers.IO) {
            runCatching { json.decodeFromString<List<BackupInfo>>(app.engine.backups(game.config.id)) }.getOrDefault(emptyList())
        }
    }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Backups of ${game.config.name}") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("SaveSync backs up this device's save before replacing it. Restoring one puts it back and syncs it as the newest version.")
                val list = backups
                when {
                    list == null -> Text("Loading…")
                    list.isEmpty() -> Hint("No backups yet. One is made before each import.")
                    else -> LazyColumn(Modifier.heightIn(max = 320.dp)) {
                        items(list) { b ->
                            Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(vertical = 4.dp)) {
                                Column(Modifier.weight(1f)) {
                                    Text(java.text.DateFormat.getDateTimeInstance().format(java.util.Date(b.createdAt)))
                                    Hint("${b.reason} · ${b.fileCount} file${if (b.fileCount == 1) "" else "s"}")
                                }
                                TextButton(onClick = { restore = b }) { Text("Restore") }
                            }
                        }
                    }
                }
            }
        },
        confirmButton = { TextButton(onClick = onDismiss) { Text("Close") } },
    )
    restore?.let { b ->
        ConfirmDialog(
            title = "Restore this backup?",
            text = "The current save of ${game.config.name} is backed up first, then this one is restored and synced to your other devices.",
            confirm = "Restore",
            onConfirm = { onDismiss(); actions.run(game.config.id, "Backup restored") { app.engine.restoreBackup(b.id) } },
            onDismiss = { restore = null },
        )
    }
}

// ---------------------------------------------------------------- settings

/** Bumps whenever the screen resumes, so permission states re-read after visiting system settings. */
@Composable
private fun resumeTick(): Int {
    var tick by remember { mutableIntStateOf(0) }
    val owner = LocalLifecycleOwner.current
    DisposableEffect(owner) {
        val observer = LifecycleEventObserver { _, e -> if (e == Lifecycle.Event.ON_RESUME) tick++ }
        owner.lifecycle.addObserver(observer)
        onDispose { owner.lifecycle.removeObserver(observer) }
    }
    return tick
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(s: Snapshot, app: SaveSyncApp, activity: Activity, actions: Actions, onBack: () -> Unit) {
    val context = LocalContext.current
    val tick = resumeTick()
    val usage = remember(tick) { SessionWatcher.hasUsageAccess(context) }
    val notifications = remember(tick) { NotificationManagerCompat.from(context).areNotificationsEnabled() }
    val battery = remember(tick) { context.getSystemService(PowerManager::class.java).isIgnoringBatteryOptimizations(context.packageName) }
    val push = remember(tick) { UnifiedPush.getAckDistributor(context) }
    val distributors = remember(tick) { UnifiedPush.getDistributors(context) }
    var url by remember { mutableStateOf(s.server?.url ?: "") }
    var confirmUnpair by remember { mutableStateOf(false) }
    val askNotifications = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {}

    Scaffold(
        topBar = { TopAppBar(title = { Text("Settings") }, navigationIcon = { TextButton(onClick = onBack) { Text("Back") } }) },
        snackbarHost = { SnackbarHost(actions.snackbar) },
    ) { padding ->
        Column(
            Modifier.fillMaxSize().padding(padding).verticalScroll(rememberScrollState()).padding(horizontal = 20.dp, vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Section("Background syncing")
            Hint("SaveSync works best with all of these on.")
            if (Build.VERSION.SDK_INT >= 37) {
                val localNetwork = remember(tick) { !needsLocalNetworkPermission(context) }
                val askLocalNetwork = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {}
                Setting("Local network", if (localNetwork) "On" else "Off: needed to reach a NAS on your home network.", localNetwork) {
                    askLocalNetwork.launch(LOCAL_NETWORK)
                }
            }
            Setting("Usage access", if (usage) "On: SaveSync knows when an emulator is open." else "Off: needed to notice when you start and stop playing.", usage) {
                context.startActivity(Intent(Settings.ACTION_USAGE_ACCESS_SETTINGS))
            }
            Setting("Notifications", if (notifications) "On" else "Off: you won't hear about new saves or conflicts.", notifications) {
                if (Build.VERSION.SDK_INT >= 33 && ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
                    askNotifications.launch(Manifest.permission.POST_NOTIFICATIONS)
                } else {
                    context.startActivity(Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS).putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName))
                }
            }
            Setting("Unrestricted battery", if (battery) "On" else "Off: Android may pause syncing in the background.", battery) {
                context.startActivity(Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:${context.packageName}")))
            }
            Setting(
                "Instant updates",
                when {
                    push != null -> "On, through $push."
                    distributors.isEmpty() -> "Install the ntfy app and set its server to ${s.server?.url?.replace(":8420", ":8421")} to get new saves within seconds."
                    else -> "Off: tap to use ${distributors.first()}."
                },
                push != null,
            ) { if (distributors.isNotEmpty()) registerPush(activity, app) }

            HorizontalDivider(Modifier.padding(vertical = 8.dp))
            Section("Appearance")
            val theme by ThemePrefs.mode.collectAsStateWithLifecycle()
            Choice(theme == "system", "System default", "Follow your phone's light/dark setting.") { ThemePrefs.set(context, "system") }
            Choice(theme == "light", "Light", "Always light.") { ThemePrefs.set(context, "light") }
            Choice(theme == "dark", "Dark", "Always dark.") { ThemePrefs.set(context, "dark") }

            HorizontalDivider(Modifier.padding(vertical = 8.dp))
            Section("Updates")
            val update by com.nicklbhender.savesync.Updater.state.collectAsStateWithLifecycle()
            val current = remember { com.nicklbhender.savesync.Updater.currentVersion(context) }
            val scope = rememberCoroutineScope()
            Text(
                "SaveSync $current · " + when (val u = update) {
                    com.nicklbhender.savesync.Updater.State.Checking -> "checking…"
                    com.nicklbhender.savesync.Updater.State.UpToDate -> "up to date"
                    is com.nicklbhender.savesync.Updater.State.Downloading -> "downloading ${u.version}…"
                    is com.nicklbhender.savesync.Updater.State.Ready ->
                        "version ${u.version} is ready" + (u.problem?.let { " (last attempt: $it)" } ?: "")
                    is com.nicklbhender.savesync.Updater.State.Error -> "update problem: ${u.message}"
                    com.nicklbhender.savesync.Updater.State.Idle -> "checks for updates automatically"
                },
            )
            if (update is com.nicklbhender.savesync.Updater.State.Ready) {
                Button(onClick = { com.nicklbhender.savesync.Updater.install(activity) }) { Text("Install update") }
                Hint("Android asks you to confirm. The first time, allow SaveSync to install updates.")
            } else {
                OutlinedButton(onClick = { scope.launch { com.nicklbhender.savesync.Updater.check(context, force = true) } }) { Text("Check now") }
            }

            HorizontalDivider(Modifier.padding(vertical = 8.dp))
            Section("Server")
            OutlinedTextField(url, { url = it }, label = { Text("Address") }, singleLine = true, modifier = Modifier.fillMaxWidth(),
                supportingText = { Text("Change this if your NAS's address changes.") })
            OutlinedButton(onClick = { actions.run("url", "Server address saved") { app.engine.setServerUrl(url.trim()) } }) { Text("Save address") }

            HorizontalDivider(Modifier.padding(vertical = 8.dp))
            Section("This device")
            Text("${s.server?.deviceName}", fontWeight = FontWeight.Medium)
            Hint("Device ID ${s.server?.deviceId}")
            TextButton(onClick = { confirmUnpair = true }) { Text("Disconnect from the server", color = MaterialTheme.colorScheme.error) }
            Spacer(Modifier.heightIn(min = 24.dp))
        }
    }
    if (confirmUnpair) {
        ConfirmDialog(
            title = "Disconnect from the server?",
            text = "This device stops syncing until you connect again. Nothing is deleted.",
            confirm = "Disconnect",
            onConfirm = { actions.run("unpair", onDone = onBack) { app.engine.unpair() } },
            onDismiss = { confirmUnpair = false },
        )
    }
}

@Composable
private fun Setting(title: String, detail: String, ok: Boolean, onClick: () -> Unit) {
    Row(Modifier.fillMaxWidth().clickable(onClick = onClick).padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            Text(title, fontWeight = FontWeight.Medium)
            Hint(detail)
        }
        Badge(if (ok) "On" else "Set up", if (ok) "synced" else "queued")
    }
}
