package com.nicklbhender.savesync

import android.content.ContentResolver
import android.content.Context
import android.database.Cursor
import android.net.Uri
import android.provider.DocumentsContract
import android.provider.DocumentsContract.Document
import uniffi.savesync_ffi.FfiException
import uniffi.savesync_ffi.FolderAccess
import uniffi.savesync_ffi.FolderEntry

/**
 * Save-folder access for the Rust engine through the Storage Access Framework.
 * Each game's location is a tree URI the user picked (with persisted permission);
 * paths inside it are `/`-separated.
 */
class SafFolderAccess(private val context: Context) : FolderAccess {
    private val resolver: ContentResolver get() = context.contentResolver

    private data class Doc(val id: String, val name: String, val isDir: Boolean, val size: Long, val mtime: Long)

    private val projection = arrayOf(
        Document.COLUMN_DOCUMENT_ID,
        Document.COLUMN_DISPLAY_NAME,
        Document.COLUMN_MIME_TYPE,
        Document.COLUMN_SIZE,
        Document.COLUMN_LAST_MODIFIED,
    )

    private fun tree(location: String): Pair<Uri, String> {
        val tree = Uri.parse(location)
        val rootId = try {
            DocumentsContract.getTreeDocumentId(tree)
        } catch (e: IllegalArgumentException) {
            throw FfiException.Failed("Not a folder SaveSync can open: $location")
        }
        return tree to rootId
    }

    private fun children(tree: Uri, parentId: String): List<Doc> {
        val uri = DocumentsContract.buildChildDocumentsUriUsingTree(tree, parentId)
        val cursor: Cursor = try {
            resolver.query(uri, projection, null, null, null)
        } catch (e: SecurityException) {
            throw FfiException.NotFound("SaveSync lost access to the save folder. Choose it again in the game's settings.")
        } catch (e: Exception) {
            throw FfiException.NotFound("Save folder not found (${e.message})")
        } ?: throw FfiException.NotFound("Save folder not found")
        return cursor.use {
            buildList {
                while (it.moveToNext()) {
                    add(
                        Doc(
                            id = it.getString(0),
                            name = it.getString(1) ?: continue,
                            isDir = it.getString(2) == Document.MIME_TYPE_DIR,
                            size = if (it.isNull(3)) 0 else it.getLong(3),
                            mtime = if (it.isNull(4)) 0 else it.getLong(4),
                        )
                    )
                }
            }
        }
    }

    override fun list(location: String): List<FolderEntry> {
        val (tree, rootId) = tree(location)
        val out = mutableListOf<FolderEntry>()
        fun walk(parentId: String, prefix: String) {
            for (doc in children(tree, parentId)) {
                val path = if (prefix.isEmpty()) doc.name else "$prefix/${doc.name}"
                if (doc.isDir) walk(doc.id, path) else out += FolderEntry(path, doc.size.toULong(), doc.mtime)
            }
        }
        walk(rootId, "")
        return out
    }

    /** Document id of `path`'s parent folder (created if asked), and the file name. */
    private fun parentOf(tree: Uri, rootId: String, path: String, create: Boolean): Pair<String, String>? {
        val parts = path.split('/')
        // The engine validates paths already; refuse anything that could leave the folder regardless.
        if (path.startsWith("/") || '\\' in path || parts.any { it.isEmpty() || it == "." || it == ".." }) {
            throw FfiException.Failed("Refused unsafe path: $path")
        }
        var dirId = rootId
        for (dir in parts.dropLast(1)) {
            val existing = children(tree, dirId).firstOrNull { it.isDir && it.name == dir }
            dirId = when {
                existing != null -> existing.id
                create -> {
                    val parentUri = DocumentsContract.buildDocumentUriUsingTree(tree, dirId)
                    val created = DocumentsContract.createDocument(resolver, parentUri, Document.MIME_TYPE_DIR, dir)
                        ?: throw FfiException.Failed("Couldn't create folder $dir")
                    DocumentsContract.getDocumentId(created)
                }
                else -> return null
            }
        }
        return dirId to parts.last()
    }

    private fun find(tree: Uri, rootId: String, path: String): Uri? {
        val (dirId, name) = parentOf(tree, rootId, path, create = false) ?: return null
        val doc = children(tree, dirId).firstOrNull { !it.isDir && it.name == name } ?: return null
        return DocumentsContract.buildDocumentUriUsingTree(tree, doc.id)
    }

    override fun read(location: String, path: String): ByteArray {
        val (tree, rootId) = tree(location)
        val uri = find(tree, rootId, path) ?: throw FfiException.NotFound("$path not found")
        return resolver.openInputStream(uri)?.use { it.readBytes() }
            ?: throw FfiException.Failed("Couldn't read $path")
    }

    /**
     * Writes to a temporary file first, then swaps it in, so an interrupted write
     * never leaves a half-written save behind.
     */
    override fun write(location: String, path: String, data: ByteArray) {
        val (tree, rootId) = tree(location)
        val (dirId, name) = parentOf(tree, rootId, path, create = true)!!
        val dirUri = DocumentsContract.buildDocumentUriUsingTree(tree, dirId)
        val tmp = DocumentsContract.createDocument(
            resolver, dirUri, "application/octet-stream", ".savesync-${System.nanoTime()}.tmp",
        ) ?: throw FfiException.Failed("Couldn't write $path")
        try {
            resolver.openOutputStream(tmp, "wt")?.use { it.write(data) }
                ?: throw FfiException.Failed("Couldn't write $path")
            children(tree, dirId).firstOrNull { !it.isDir && it.name == name }?.let {
                DocumentsContract.deleteDocument(resolver, DocumentsContract.buildDocumentUriUsingTree(tree, it.id))
            }
            if (DocumentsContract.renameDocument(resolver, tmp, name) == null) {
                throw FfiException.Failed("Couldn't rename the new $path into place")
            }
        } catch (e: Exception) {
            runCatching { DocumentsContract.deleteDocument(resolver, tmp) }
            throw e as? FfiException ?: FfiException.Failed("Couldn't write $path: ${e.message}")
        }
    }

    override fun remove(location: String, path: String) {
        val (tree, rootId) = tree(location)
        val uri = find(tree, rootId, path) ?: return
        DocumentsContract.deleteDocument(resolver, uri)
    }
}
