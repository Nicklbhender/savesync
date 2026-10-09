package com.nicklbhender.savesync

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

// Mirrors of the engine's serde types (engine/src/types.rs, protocol/src/lib.rs).

val json = Json {
    ignoreUnknownKeys = true
    explicitNulls = false
    encodeDefaults = true
}

@Serializable
data class ServerInfo(
    val url: String,
    @SerialName("device_id") val deviceId: String,
    @SerialName("device_name") val deviceName: String,
)

@Serializable
data class VersionInfo(
    @SerialName("game_id") val gameId: String,
    val version: Long,
    @SerialName("device_name") val deviceName: String? = null,
    @SerialName("created_at") val createdAt: Long,
    @SerialName("played_at") val playedAt: Long? = null,
    @SerialName("total_size") val totalSize: Long = 0,
) {
    val from: String get() = deviceName ?: "another device"
    val playedOrCreated: Long get() = playedAt ?: createdAt
}

@Serializable
data class GameConfig(
    val id: String,
    val name: String,
    val location: String,
    val include: List<String> = emptyList(),
    val ignore: List<String> = emptyList(),
    val processes: List<String> = emptyList(),
    @SerialName("import_policy") val importPolicy: String = "ask",
)

@Serializable
data class ConflictInfo(
    val remote: VersionInfo,
    @SerialName("local_played_at") val localPlayedAt: Long? = null,
)

@Serializable
data class QueuedUpload(
    @SerialName("played_at") val playedAt: Long? = null,
    @SerialName("created_at") val createdAt: Long,
    val blocked: Boolean = false,
)

@Serializable
data class GameView(
    val config: GameConfig,
    @SerialName("base_version") val baseVersion: Long,
    val registered: Boolean = false,
    @SerialName("in_session") val inSession: Boolean = false,
    @SerialName("queued_upload") val queuedUpload: QueuedUpload? = null,
    val staged: VersionInfo? = null,
    val conflict: ConflictInfo? = null,
    @SerialName("last_error") val lastError: String? = null,
)

@Serializable
data class Snapshot(
    val server: ServerInfo? = null,
    val online: Boolean = false,
    val games: List<GameView> = emptyList(),
)

@Serializable
data class BackupInfo(
    val id: Long,
    @SerialName("created_at") val createdAt: Long,
    val reason: String,
    @SerialName("file_count") val fileCount: Int,
    @SerialName("total_size") val totalSize: Long,
)

/** Any engine event; fields are present depending on [type]. */
@Serializable
data class EngineEvent(
    val type: String,
    @SerialName("game_id") val gameId: String? = null,
    @SerialName("game_name") val gameName: String? = null,
    val version: Long? = null,
    val created: Boolean? = null,
    val online: Boolean? = null,
    val message: String? = null,
    val remote: VersionInfo? = null,
    val conflict: ConflictInfo? = null,
)
