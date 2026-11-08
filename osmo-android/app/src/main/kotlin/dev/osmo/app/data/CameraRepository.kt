package dev.osmo.app.data

import android.annotation.SuppressLint
import android.app.Application
import android.content.ContentValues
import android.os.Build
import android.provider.MediaStore
import android.util.LruCache
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit
import java.util.concurrent.locks.ReentrantReadWriteLock
import kotlin.concurrent.read
import kotlin.concurrent.write
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import uniffi.osmo_ffi.Camera
import uniffi.osmo_ffi.ListFilter
import uniffi.osmo_ffi.MediaFile
import uniffi.osmo_ffi.MediaKind
import uniffi.osmo_ffi.OsmoException
import uniffi.osmo_ffi.ProgressListener
import uniffi.osmo_ffi.TrimResult
import uniffi.osmo_ffi.readWifiCredentials
import uniffi.osmo_ffi.trimLocalFile

sealed interface Link {
    data object Idle : Link
    data class Working(val step: String, val needsApproval: Boolean = false) : Link
    data class Connected(val name: String) : Link
    data class Failed(val message: String) : Link
}

/** Human-readable text for any SDK error. */
fun Throwable.friendly(): String = when (this) {
    is OsmoException.Cancelled -> "Cancelled"
    is OsmoException.Session -> "Camera session: $detail"
    is OsmoException.NotFound -> "Not found: $detail"
    is OsmoException.Refused -> "Refused: $detail"
    is OsmoException.Io -> "I/O: $detail"
    is OsmoException.Mp4 -> "Video file: $detail"
    is OsmoException.Ble -> "Bluetooth: $detail"
    is OsmoException.Internal -> "Internal error: $detail"
    else -> message ?: toString()
}

const val CAMERA_IP = "192.168.2.1"
const val CAMERA_PORT: UShort = 9004u

class CameraRepository(private val app: Application) {
    private val store = CameraStore(app)
    val scanner = BleScanner(app)
    private val wifi = WifiBinder(app)

    private val _link = MutableStateFlow<Link>(Link.Idle)
    val link: StateFlow<Link> = _link.asStateFlow()
    private val _saved = MutableStateFlow(store.load())
    val saved: StateFlow<SavedCamera?> = _saved.asStateFlow()

    @Volatile private var camera: Camera? = null
    /**
     * The camera allows one datalink session. Operations take the write lock (and end any browse session);
     * thumbnails and playback share the read lock on one kept-alive browse session, so they run in parallel.
     */
    private val rw = ReentrantReadWriteLock()
    private val thumbPermits = Semaphore(6)
    private val thumbs = object : LruCache<String, ByteArray>(24 * 1024 * 1024) {
        override fun sizeOf(key: String, value: ByteArray) = value.size
    }

    val mediaDir: File get() = File(app.getExternalFilesDir(null) ?: app.filesDir, "osmo").apply { mkdirs() }

    /** Wake the camera over Bluetooth (approve on its screen the first time), join its Wi-Fi and open the session. */
    @SuppressLint("MissingPermission")
    suspend fun connect(target: FoundCamera?) = withContext(Dispatchers.IO) {
        disconnectInternal()
        try {
            val device = target?.device ?: store.load()?.let { s ->
                scanner.adapter?.getRemoteDevice(s.address)
            } ?: throw IllegalStateException("No camera selected")
            val name = target?.name ?: store.load()?.name ?: "Osmo"
            _link.value = Link.Working("Connecting over Bluetooth")
            val creds = GattLink(app, device).use { gatt ->
                gatt.open()
                _link.value = Link.Working("Approve on the camera if it asks", needsApproval = true)
                readWifiCredentials(gatt)
            }
            store.save(SavedCamera(device.address, name)).also { _saved.value = store.load() }
            _link.value = Link.Working("Joining ${creds.ssid}")
            wifi.join(creds.ssid, creds.password)
            camera = Camera(CAMERA_IP, CAMERA_PORT)
            _link.value = Link.Connected(name)
        } catch (e: Throwable) {
            disconnectInternal()
            _link.value = Link.Failed(e.friendly())
        }
    }

    fun disconnect() {
        disconnectInternal()
        _link.value = Link.Idle
    }

    private fun disconnectInternal() {
        camera?.cancel()
        camera?.close()
        camera = null
        wifi.release()
        thumbs.evictAll()
    }

    fun forget() {
        disconnect()
        store.forget()
        _saved.value = null
    }

    fun cancel() { camera?.cancel() }

    private fun cam(): Camera = camera ?: throw OsmoException.Session("Not connected to a camera")

    private suspend fun <T> op(block: (Camera) -> T): T = withContext(Dispatchers.IO) { rw.write { block(cam()) } }

    suspend fun list(kind: MediaKind?, starredOnly: Boolean): List<MediaFile> =
        op { it.list(ListFilter(kind, starredOnly), true) }

    /** Cached; misses fetch over HTTP on the shared browse session, up to 6 at a time. */
    suspend fun thumbnail(file: MediaFile): ByteArray? {
        thumbs.get(file.path)?.let { return it }
        return thumbPermits.withPermit {
            thumbs.get(file.path) ?: runCatching {
                withContext(Dispatchers.IO) {
                    rw.read { cam().let { it.browseStart(); it.thumbnailFast(file.path, file.storage) } }
                }
            }.getOrNull()?.also { thumbs.put(file.path, it) }
        }
    }

    /** URL to stream [file] (or its low-res proxy, if [proxy]) from the camera. Keeps the browse session open. */
    suspend fun streamUrl(file: MediaFile, proxy: Boolean): String = withContext(Dispatchers.IO) {
        rw.read { cam().browseStart() }
        val path = if (proxy) file.path.substringBeforeLast('.') + ".LRF" else file.path
        "http://$CAMERA_IP/v2?storage=${file.storage}&path=$path"
    }

    suspend fun setFavourite(path: String, on: Boolean) = op { it.setFavourite(path, on) }

    suspend fun planDelete(paths: List<String>): List<MediaFile> = op { it.planDelete(paths) }
    suspend fun delete(paths: List<String>) = op { it.delete(paths) }

    /** Downloads into app storage (resumes partial files), then copies into the gallery. Returns the local file. */
    suspend fun download(file: MediaFile, onProgress: (Long, Long) -> Unit): File = op { c ->
        val saved = File(c.download(file.path, mediaDir.path, progress(onProgress)))
        publish(saved, file.kind)
        saved
    }

    suspend fun trim(file: MediaFile, fromSecs: Double, toSecs: Double, onProgress: (Long, Long) -> Unit): Pair<File, TrimResult> = op { c ->
        val base = file.path.substringAfterLast('/').substringBeforeLast('.')
        val out = File(mediaDir, "${base}_${fromSecs.toInt()}-${toSecs.toInt()}s.mp4")
        val r = c.trim(file.path, fromSecs, toSecs, out.path, progress(onProgress))
        publish(out, MediaKind.VIDEO)
        out to r
    }

    /** Extracts a still at [atSecs] by cutting a half-second clip and decoding its first frame. */
    suspend fun frame(file: MediaFile, atSecs: Double): File = op { c ->
        val base = file.path.substringAfterLast('/').substringBeforeLast('.')
        val clip = File(app.cacheDir, "${base}_frame.mp4")
        c.frameClip(file.path, atSecs, clip.path)
        val out = File(mediaDir, "${base}_${(atSecs * 1000).toLong()}ms.jpg")
        val mmr = android.media.MediaMetadataRetriever()
        try {
            mmr.setDataSource(clip.path)
            val bmp = mmr.getFrameAtTime(0, android.media.MediaMetadataRetriever.OPTION_CLOSEST_SYNC)
                ?: throw OsmoException.Mp4("could not decode a frame")
            out.outputStream().use { bmp.compress(android.graphics.Bitmap.CompressFormat.JPEG, 95, it) }
        } finally {
            mmr.release(); clip.delete()
        }
        publish(out, MediaKind.PHOTO)
        out
    }

    /** Stars or un-stars several files on the camera, one after another. Returns how many succeeded. */
    suspend fun setFavouriteMany(paths: List<String>, on: Boolean): Int = op { c ->
        paths.count { p -> runCatching { c.setFavourite(p, on) }.isSuccess }
    }

    /** Downloads several files in order. [onProgress] gets (index, count, bytes done, bytes total) for the current file. */
    suspend fun downloadMany(files: List<MediaFile>, onProgress: (Int, Int, Long, Long) -> Unit): List<File> = op { c ->
        files.mapIndexed { i, f ->
            val saved = File(c.download(f.path, mediaDir.path, progress { d, t -> onProgress(i, files.size, d, t) }))
            publish(saved, f.kind)
            saved
        }
    }

    // --- Local files: work without a camera connection -------------------------------------

    /** Everything this app has saved, newest first. */
    suspend fun localFiles(): List<File> = withContext(Dispatchers.IO) {
        mediaDir.listFiles { f -> f.isFile && !f.name.endsWith(".part") }?.sortedByDescending { it.lastModified() } ?: emptyList()
    }

    suspend fun deleteLocal(file: File) = withContext(Dispatchers.IO) { file.delete() }

    /** Trims a file already on the phone (no camera needed). */
    suspend fun trimLocal(src: File, fromSecs: Double, toSecs: Double, onProgress: (Long, Long) -> Unit): Pair<File, TrimResult> =
        withContext(Dispatchers.IO) {
            val out = File(mediaDir, "${src.nameWithoutExtension}_${fromSecs.toInt()}-${toSecs.toInt()}s.mp4")
            val r = trimLocalFile(src.path, fromSecs, toSecs, out.path, progress(onProgress))
            publish(out, MediaKind.VIDEO)
            out to r
        }

    /** A content:// URI other apps can read, for open/share intents. */
    fun uriFor(file: File): android.net.Uri =
        androidx.core.content.FileProvider.getUriForFile(app, "${app.packageName}.files", file)

    private fun progress(on: (Long, Long) -> Unit) = object : ProgressListener {
        override fun onProgress(done: ULong, total: ULong): Boolean {
            on(done.toLong(), total.toLong()); return true
        }
    }

    private fun publish(file: File, kind: MediaKind) {
        if (Build.VERSION.SDK_INT < 29) return
        val video = kind == MediaKind.VIDEO
        val values = ContentValues().apply {
            put(MediaStore.MediaColumns.DISPLAY_NAME, file.name)
            put(MediaStore.MediaColumns.MIME_TYPE, if (video) "video/mp4" else if (file.extension.equals("jpg", true) || file.extension.equals("jpeg", true)) "image/jpeg" else "image/*")
            put(MediaStore.MediaColumns.RELATIVE_PATH, (if (video) "Movies" else "Pictures") + "/Osmo")
        }
        val collection = if (video) MediaStore.Video.Media.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
        else MediaStore.Images.Media.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
        val resolver = app.contentResolver
        // Replace an earlier copy of the same name so re-downloads do not pile up.
        resolver.delete(collection, "${MediaStore.MediaColumns.DISPLAY_NAME}=? AND ${MediaStore.MediaColumns.RELATIVE_PATH} LIKE ?", arrayOf(file.name, "%/Osmo/"))
        val uri = resolver.insert(collection, values) ?: return
        resolver.openOutputStream(uri)?.use { out -> file.inputStream().use { it.copyTo(out) } }
    }
}
