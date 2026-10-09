package com.nicklbhender.savesync

import android.app.Activity
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInfo
import android.content.pm.PackageInstaller
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.provider.Settings
import android.util.Log
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerialName

/**
 * Self-update from GitHub Releases. Downloads the new APK, checks it against the
 * release's SHA256SUMS and against this app's own signing certificate (Android
 * also refuses any update signed with a different key), then asks to install.
 */
object Updater {
    private const val CHECK_EVERY_MS = 6 * 60 * 60 * 1000L
    private const val PREFS = "updates"

    sealed interface State {
        data object Idle : State
        data object Checking : State
        data object UpToDate : State
        data class Downloading(val version: String) : State
        /** Downloaded and verified. [problem] is set if an install attempt failed (it can be retried). */
        data class Ready(val version: String, val file: File, val problem: String? = null) : State
        data class Error(val message: String) : State
    }

    private val _state = MutableStateFlow<State>(State.Idle)
    val state: StateFlow<State> = _state
    private val lock = Mutex()

    @Serializable
    private data class Release(
        @SerialName("tag_name") val tagName: String,
        val draft: Boolean = false,
        val prerelease: Boolean = false,
        val assets: List<Asset> = emptyList(),
    )

    @Serializable
    private data class Asset(val name: String, @SerialName("browser_download_url") val url: String)

    fun currentVersion(context: Context): String =
        context.packageManager.getPackageInfo(context.packageName, 0).versionName ?: "0.0.0"

    private fun parse(v: String): List<Int>? =
        v.trim().removePrefix("v").split('.').takeIf { it.size == 3 }?.map { it.toIntOrNull() ?: return null }

    fun isNewer(candidate: String, current: String): Boolean {
        val a = parse(candidate) ?: return false
        val b = parse(current) ?: return false
        for (i in 0..2) if (a[i] != b[i]) return a[i] > b[i]
        return false
    }

    private fun get(url: String): HttpURLConnection = (URL(url).openConnection() as HttpURLConnection).apply {
        setRequestProperty("User-Agent", "SaveSync-Android")
        setRequestProperty("Accept", "application/vnd.github+json")
        connectTimeout = 15_000
        readTimeout = 60_000
        instanceFollowRedirects = true
    }

    /** Checks at most every few hours unless [force]d; downloads a newer version if there is one. */
    suspend fun check(context: Context, force: Boolean = false) = withContext(Dispatchers.IO) {
        if (!lock.tryLock()) return@withContext
        try {
            val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            val now = System.currentTimeMillis()
            if (_state.value is State.Ready) return@withContext
            if (!force && now - prefs.getLong("last_check", 0) < CHECK_EVERY_MS) return@withContext
            _state.value = State.Checking
            _state.value = try {
                fetch(context).also {
                    // Only successful checks count, so a failed one (e.g. offline) retries next time.
                    prefs.edit().putLong("last_check", now).apply()
                }
            } catch (e: Exception) {
                Log.w(SaveSyncApp.TAG, "update check failed", e)
                State.Error(e.message ?: "Update check failed")
            }
            (_state.value as? State.Ready)?.let { Notifications.updateReady(context, it.version) }
        } finally {
            lock.unlock()
        }
    }

    private fun fetch(context: Context): State {
        val conn = get(BuildConfig.UPDATE_API)
        if (conn.responseCode == 404) return State.UpToDate
        if (conn.responseCode != 200) throw IllegalStateException("GitHub returned ${conn.responseCode}")
        val release = json.decodeFromString<Release>(conn.inputStream.bufferedReader().use { it.readText() })
        val version = release.tagName.removePrefix("v")
        if (release.draft || release.prerelease || !isNewer(version, currentVersion(context))) return State.UpToDate

        val apkName = "SaveSync-$version-android.apk"
        val apk = release.assets.firstOrNull { it.name == apkName }
        val sums = release.assets.firstOrNull { it.name == "SHA256SUMS" }
        if (apk == null || sums == null) throw IllegalStateException("Release $version is missing $apkName or its checksums")
        val expected = get(sums.url).inputStream.bufferedReader().use { it.readText() }.lines().firstNotNullOfOrNull { line ->
            val parts = line.trim().split(Regex("\\s+"), limit = 2)
            if (parts.size == 2 && parts[1].trimStart('*') == apkName) parts[0].lowercase() else null
        } ?: throw IllegalStateException("The release's checksums don't list $apkName")

        _state.value = State.Downloading(version)
        val dir = File(context.cacheDir, "updates").apply { deleteRecursively(); mkdirs() }
        val file = File(dir, apkName)
        val digest = MessageDigest.getInstance("SHA-256")
        get(apk.url).inputStream.use { input ->
            file.outputStream().use { out ->
                val buf = ByteArray(64 * 1024)
                while (true) {
                    val n = input.read(buf)
                    if (n < 0) break
                    digest.update(buf, 0, n)
                    out.write(buf, 0, n)
                }
            }
        }
        val actual = digest.digest().joinToString("") { "%02x".format(it) }
        if (actual != expected) {
            file.delete()
            throw IllegalStateException("The downloaded update didn't match its checksum")
        }
        if (!sameSigner(context, file)) {
            file.delete()
            throw IllegalStateException("The update isn't signed with SaveSync's key")
        }
        return State.Ready(version, file)
    }

    @Suppress("DEPRECATION")
    private fun signatures(info: PackageInfo?): Set<String> {
        if (info == null) return emptySet()
        val sigs = if (Build.VERSION.SDK_INT >= 28) info.signingInfo?.apkContentsSigners else info.signatures
        return sigs.orEmpty().map { it.toCharsString() }.toSet()
    }

    @Suppress("DEPRECATION")
    private fun sameSigner(context: Context, apk: File): Boolean {
        val pm = context.packageManager
        val flags = if (Build.VERSION.SDK_INT >= 28) PackageManager.GET_SIGNING_CERTIFICATES else PackageManager.GET_SIGNATURES
        val installed = signatures(pm.getPackageInfo(context.packageName, flags))
        val candidate = signatures(pm.getPackageArchiveInfo(apk.path, flags))
        return installed.isNotEmpty() && installed == candidate
    }

    /** Starts installing the downloaded update; Android shows its confirmation. */
    fun install(activity: Activity) {
        val ready = _state.value as? State.Ready ?: return
        val pm = activity.packageManager
        if (!pm.canRequestPackageInstalls()) {
            // One-time: let SaveSync install its own updates.
            activity.startActivity(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${activity.packageName}")))
            return
        }
        val installer = pm.packageInstaller
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL).apply {
            setAppPackageName(activity.packageName)
            if (Build.VERSION.SDK_INT >= 31) setRequireUserAction(PackageInstaller.SessionParams.USER_ACTION_NOT_REQUIRED)
        }
        val id = installer.createSession(params)
        installer.openSession(id).use { session ->
            session.openWrite("base.apk", 0, ready.file.length()).use { out ->
                ready.file.inputStream().use { it.copyTo(out) }
                session.fsync(out)
            }
            val callback = PendingIntent.getBroadcast(
                activity, id, Intent(activity, InstallResultReceiver::class.java),
                PendingIntent.FLAG_MUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
            session.commit(callback.intentSender)
        }
    }

    internal fun failed(message: String) {
        _state.value = (_state.value as? State.Ready)?.copy(problem = message) ?: State.Error(message)
    }
}

/** Receives the installer's result; shows Android's confirmation screen when it asks for one. */
class InstallResultReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        when (val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)) {
            PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                @Suppress("DEPRECATION")
                val confirm = if (Build.VERSION.SDK_INT >= 33) intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
                else intent.getParcelableExtra(Intent.EXTRA_INTENT)
                confirm?.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)?.let { context.startActivity(it) }
            }
            PackageInstaller.STATUS_SUCCESS -> Log.i(SaveSyncApp.TAG, "update installed")
            else -> {
                val message = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE) ?: "status $status"
                Log.w(SaveSyncApp.TAG, "update install failed: $message")
                Updater.failed("Install failed: $message")
            }
        }
    }
}
