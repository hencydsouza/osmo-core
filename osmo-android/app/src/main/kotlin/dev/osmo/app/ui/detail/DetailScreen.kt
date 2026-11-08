@file:OptIn(androidx.compose.material3.ExperimentalMaterial3ExpressiveApi::class, androidx.compose.foundation.layout.ExperimentalLayoutApi::class)

package dev.osmo.app.ui.detail

import android.graphics.BitmapFactory
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Download
import androidx.compose.material.icons.filled.Info
import androidx.compose.material.icons.filled.Star
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.StarBorder
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearWavyProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.filled.CameraAlt
import androidx.compose.material.icons.filled.ContentCut
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.AssistChip
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.IconToggleButton
import androidx.compose.material3.LoadingIndicator
import androidx.compose.material3.RangeSlider
import androidx.compose.material3.Surface
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.viewinterop.AndroidView
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.ui.PlayerView
import kotlinx.coroutines.delay
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import dev.osmo.app.data.CameraRepository
import dev.osmo.app.data.friendly
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.osmo_ffi.MediaFile
import uniffi.osmo_ffi.MediaKind

data class DetailState(
    val starred: Boolean,
    val busy: String? = null,
    val done: Long = 0,
    val total: Long = 0,
    val message: String? = null,
    val deletePlan: List<MediaFile>? = null,
    val deleted: Boolean = false,
)

class DetailViewModel(private val repo: CameraRepository, val file: MediaFile) : ViewModel() {
    private val _s = MutableStateFlow(DetailState(starred = file.starred))
    val state = _s
    private var job: Job? = null
    suspend fun thumbnail() = repo.thumbnail(file)
    suspend fun streamUrl(proxy: Boolean) = repo.streamUrl(file, proxy)
    suspend fun highlights(): List<UInt> = runCatching { repo.highlights(file.path) }.getOrDefault(emptyList())
    val sidecars = repo.sidecars
    fun setSidecars(on: Boolean) = repo.setSidecars(on)

    private fun run(label: String, block: suspend () -> String) {
        if (job?.isActive == true) return
        _s.update { it.copy(busy = label, done = 0, total = 0, message = null) }
        job = viewModelScope.launch {
            val msg = runCatching { block() }.getOrElse { it.friendly() }
            _s.update { it.copy(busy = null, message = msg) }
        }
    }

    private fun progress(): (Long, Long) -> Unit = { d, t -> _s.update { it.copy(done = d, total = t) } }

    fun cancel() = repo.cancel()

    fun toggleStar() = run("Updating favourite") {
        val on = !_s.value.starred
        repo.setFavourite(file.path, on)
        _s.update { it.copy(starred = on) }
        if (on) "Starred" else "Un-starred"
    }

    fun download() = run("Downloading") {
        val f = repo.download(file, progress())
        "Saved to ${if (file.kind == MediaKind.VIDEO) "Movies" else "Pictures"}/Osmo (${f.name})"
    }

    fun trim(from: Double, to: Double) = run("Trimming") {
        val (f, r) = repo.trim(file, from, to, progress())
        "Saved ${f.name}: ${"%.1f".format(r.durationSecs)}s from ${"%.1f".format(r.actualStartSecs)}s"
    }

    fun planDelete() = run("Checking delete") {
        val plan = repo.planDelete(listOf(file.path))
        _s.update { it.copy(deletePlan = plan) }
        if (plan.isEmpty()) "Nothing to delete" else ""
    }

    fun cancelDelete() = _s.update { it.copy(deletePlan = null) }

    fun confirmDelete() {
        val paths = _s.value.deletePlan?.map { it.path } ?: return
        _s.update { it.copy(deletePlan = null) }
        run("Deleting") {
            repo.delete(paths)
            _s.update { it.copy(deleted = true) }
            "Deleted"
        }
    }

    fun frame(at: Double) = run("Extracting frame") {
        "Saved ${repo.frame(file, at).name}"
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DetailScreen(vm: DetailViewModel, active: Boolean = true, position: String = "", onBack: () -> Unit) {
    val s by vm.state.collectAsStateWithLifecycle()
    val snack = remember { SnackbarHostState() }
    LaunchedEffect(s.message) { s.message?.takeIf { it.isNotEmpty() }?.let { snack.showSnackbar(it) } }
    LaunchedEffect(s.deleted) { if (s.deleted) onBack() }
    val thumb by produceState<ImageBitmap?>(null) {
        value = vm.thumbnail()?.let { b -> BitmapFactory.decodeByteArray(b, 0, b.size)?.asImageBitmap() }
    }
    val video = vm.file.kind == MediaKind.VIDEO
    val idle = s.busy == null
    val scheme = MaterialTheme.colorScheme
    val ctx = LocalContext.current

    // Streaming preview: the camera's low-res proxy first (small, plays over Wi-Fi), the original if there is none.
    var player by remember { mutableStateOf<ExoPlayer?>(null) }
    var playerError by remember { mutableStateOf<String?>(null) }
    var durationMs by remember { mutableLongStateOf(0L) }
    var positionMs by remember { mutableLongStateOf(0L) }
    var playing by remember { mutableStateOf(false) }
    var triedOriginal by remember { mutableStateOf(false) }
    var range by remember { mutableStateOf(0f..1f) }

    var showInfo by remember { mutableStateOf(false) }
    if (video && active) {
        LaunchedEffect(Unit) {
            val p = ExoPlayer.Builder(ctx).build()
            p.addListener(object : Player.Listener {
                override fun onIsPlayingChanged(isPlaying: Boolean) { playing = isPlaying }
                override fun onPlaybackStateChanged(state: Int) {
                    if (state == Player.STATE_READY && durationMs == 0L) {
                        durationMs = p.duration.coerceAtLeast(0)
                        range = 0f..1f
                    }
                }
                override fun onPlayerError(error: PlaybackException) {
                    if (!triedOriginal) {
                        triedOriginal = true
                    } else playerError = "Can\u2019t play this clip here. You can still download it."
                }
            })
            p.setMediaItem(MediaItem.fromUri(vm.streamUrl(proxy = true)))
            p.prepare()
            player = p
        }
        LaunchedEffect(triedOriginal, player) {
            val p = player ?: return@LaunchedEffect
            if (triedOriginal) { p.setMediaItem(MediaItem.fromUri(vm.streamUrl(proxy = false))); p.prepare() }
        }
        LaunchedEffect(player) { while (true) { player?.let { positionMs = it.currentPosition }; delay(200) } }
        DisposableEffect(Unit) { onDispose { player?.release() } }
        // Camera work and playback share one Wi-Fi link: pause while a transfer runs.
        LaunchedEffect(s.busy) { if (s.busy != null) player?.pause() }
    }

    if (showInfo) DetailsSheet(vm.file, durationMs) { showInfo = false }
    s.deletePlan?.takeIf { it.isNotEmpty() }?.let { DeleteDialog(it, vm::confirmDelete, vm::cancelDelete) }

    Scaffold(
        containerColor = scheme.surface,
        snackbarHost = { SnackbarHost(snack) },
        topBar = {
            TopAppBar(
                title = { if (position.isNotEmpty()) Text(position, style = MaterialTheme.typography.titleMedium) },
                navigationIcon = { IconButton(onClick = onBack, colors = IconButtonDefaults.iconButtonColors(containerColor = MaterialTheme.colorScheme.surfaceBright)) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                actions = {
                    IconButton(onClick = { showInfo = true }) { Icon(Icons.Filled.Info, "Details") }
                    IconToggleButton(checked = s.starred, onCheckedChange = { vm.toggleStar() }, enabled = idle) {
                        Icon(if (s.starred) Icons.Filled.Star else Icons.Filled.StarBorder, if (s.starred) "Remove favourite" else "Favourite",
                            tint = if (s.starred) scheme.tertiary else scheme.onSurfaceVariant)
                    }
                    IconButton(onClick = vm::planDelete, enabled = idle) { Icon(Icons.Filled.Delete, "Delete from camera") }
                },
            )
        },
    ) { pad ->
        Column(Modifier.padding(pad).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(20.dp)) {
            // Hero: edge-to-edge media, rounded bottom (shape: my design decision, not in M3).
            Box(
                Modifier.fillMaxWidth().padding(horizontal = 16.dp).aspectRatio(16f / 9f)
                    .clip(RoundedCornerShape(28.dp)).background(scheme.surfaceContainerHigh),
                contentAlignment = Alignment.Center,
            ) {
                val p = player
                if (video && p != null && playerError == null) {
                    AndroidView(
                        factory = { c -> PlayerView(c).apply { player = p; useController = false; setShutterBackgroundColor(android.graphics.Color.TRANSPARENT) } },
                        update = { it.player = p }, modifier = Modifier.fillMaxSize(),
                    )
                }
                if (!(video && playing) || playerError != null) {
                    thumb?.let { Image(it, null, Modifier.fillMaxSize(), contentScale = ContentScale.Crop) }
                }
                if (video && playerError == null) {
                    FilledIconButton(
                        onClick = { player?.let { if (it.isPlaying) it.pause() else it.play() } },
                        Modifier.size(72.dp),
                        colors = IconButtonDefaults.filledIconButtonColors(containerColor = scheme.primaryContainer.copy(alpha = if (playing) 0.55f else 1f), contentColor = scheme.onPrimaryContainer),
                    ) { Icon(if (playing) Icons.Filled.Pause else Icons.Filled.PlayArrow, if (playing) "Pause" else "Play", Modifier.size(36.dp)) }
                    if (player != null && durationMs == 0L && !playing) {
                        LoadingIndicator(Modifier.align(Alignment.BottomEnd).padding(12.dp).size(32.dp))
                    }
                }
            }
            playerError?.let { Text(it, Modifier.padding(horizontal = 24.dp), color = scheme.error, style = MaterialTheme.typography.bodyMedium) }

            Column(Modifier.padding(horizontal = 24.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text(vm.file.path.substringAfterLast('/'), style = MaterialTheme.typography.titleLarge, maxLines = 2)
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    AssistChip(onClick = {}, label = { Text(if (video) "Video" else "Photo") })
                    AssistChip(onClick = {}, label = { Text(if (vm.file.sizeBytes.toLong() >= 1_000_000_000L) "%.2f GB".format(vm.file.sizeBytes.toLong() / 1e9) else "${vm.file.sizeBytes.toLong() / 1_000_000} MB") })
                    if (video && durationMs > 0) AssistChip(onClick = {}, label = { Text(clock(durationMs)) })
                }
            }

            s.busy?.let { label ->
                Column(Modifier.padding(horizontal = 24.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(label, style = MaterialTheme.typography.titleSmall)
                    if (s.total > 0) LinearWavyProgressIndicator(progress = { s.done.toFloat() / s.total }, Modifier.fillMaxWidth())
                    else LinearWavyProgressIndicator(Modifier.fillMaxWidth())
                    TextButton(onClick = vm::cancel) { Text("Cancel") }
                }
            }

            Button(
                onClick = vm::download, enabled = idle,
                modifier = Modifier.padding(horizontal = 24.dp).fillMaxWidth().heightIn(min = 64.dp), shape = MaterialTheme.shapes.extraLarge,
            ) {
                Icon(Icons.Filled.Download, null)
                Text("Download to phone", Modifier.padding(start = 12.dp), style = MaterialTheme.typography.titleMedium)
            }

            val withSidecars by vm.sidecars.collectAsStateWithLifecycle()
            Row(Modifier.padding(horizontal = 24.dp).fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text("Also get DNG and proxy files", style = MaterialTheme.typography.bodyLarge)
                    Text("Downloads the RAW or low-res file kept next to this one, when there is one.", style = MaterialTheme.typography.bodySmall, color = scheme.onSurfaceVariant)
                }
                androidx.compose.material3.Switch(checked = withSidecars, onCheckedChange = vm::setSidecars, enabled = idle)
            }

            if (video && durationMs > 0) {
                val total = durationMs / 1000f
                val fromS = range.start * total
                val toS = range.endInclusive * total
                Surface(Modifier.padding(horizontal = 16.dp).fillMaxWidth(), shape = RoundedCornerShape(28.dp), color = scheme.surfaceContainerLow) {
                    Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text("Trim", style = MaterialTheme.typography.titleMedium)
                        Text("Cuts at the nearest earlier keyframe. No re-encode, so it's fast and lossless.", style = MaterialTheme.typography.bodyMedium, color = scheme.onSurfaceVariant)
                        RangeSlider(
                            value = range, onValueChange = { range = it; player?.seekTo((it.start * durationMs).toLong()) },
                            enabled = idle,
                        )
                        val marks by produceState(emptyList<UInt>(), vm) { value = vm.highlights() }
                        if (marks.isNotEmpty()) {
                            Text("Highlights you marked", style = MaterialTheme.typography.labelLarge, color = scheme.onSurfaceVariant)
                            androidx.compose.foundation.layout.FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                marks.forEach { ms ->
                                    androidx.compose.material3.AssistChip(
                                        onClick = {
                                            val f = (ms.toLong().toFloat() / durationMs).coerceIn(0f, 1f)
                                            range = f.coerceAtMost(range.endInclusive - 0.01f).coerceAtLeast(0f)..range.endInclusive
                                            player?.seekTo(ms.toLong())
                                        },
                                        label = { Text(clock(ms.toLong())) },
                                        leadingIcon = { Icon(Icons.Filled.Star, null, tint = scheme.tertiary) },
                                    )
                                }
                            }
                        }
                        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                            Text(clock((fromS * 1000).toLong()), style = MaterialTheme.typography.labelLarge)
                            Text("${"%.1f".format(toS - fromS)} s", style = MaterialTheme.typography.labelLarge, color = scheme.primary)
                            Text(clock((toS * 1000).toLong()), style = MaterialTheme.typography.labelLarge)
                        }
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            FilledTonalButton(
                                onClick = { vm.trim(fromS.toDouble(), toS.toDouble()) },
                                enabled = idle && toS - fromS >= 0.5f, modifier = Modifier.weight(1f),
                            ) { Icon(Icons.Filled.ContentCut, null); Text("Save clip", Modifier.padding(start = 8.dp)) }
                            OutlinedButton(
                                onClick = { vm.frame((positionMs / 1000.0)) }, enabled = idle, modifier = Modifier.weight(1f),
                            ) { Icon(Icons.Filled.CameraAlt, null); Text("Frame here", Modifier.padding(start = 8.dp)) }
                        }
                    }
                }
            }
            Box(Modifier.height(24.dp))
        }
    }
}

private fun clock(ms: Long): String {
    val s = ms / 1000
    return "%d:%02d".format(s / 60, s % 60)
}

@Composable
private fun DeleteDialog(plan: List<MediaFile>, onConfirm: () -> Unit, onDismiss: () -> Unit) {
    androidx.compose.material3.AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Delete ${plan.size} file${if (plan.size == 1) "" else "s"} from the camera?") },
        text = { Text("This cannot be undone. Download anything you want to keep first.\n\n" + plan.joinToString("\n") { it.path.substringAfterLast('/') }) },
        confirmButton = { TextButton(onClick = onConfirm) { Text("Delete", color = MaterialTheme.colorScheme.error) } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Keep") } },
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun DetailsSheet(file: MediaFile, durationMs: Long, onDismiss: () -> Unit) {
    val scheme = MaterialTheme.colorScheme
    androidx.compose.material3.ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(Modifier.padding(horizontal = 24.dp).padding(bottom = 32.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
            Text("Details", style = MaterialTheme.typography.titleLarge)
            val size = file.sizeBytes.toLong()
            val rows = listOf(
                "Name" to file.path.substringAfterLast('/'),
                "Folder" to file.path.substringBeforeLast('/', "—"),
                "Type" to if (file.kind == MediaKind.VIDEO) "Video" else "Photo",
                "Size" to if (size >= 1_000_000_000L) "%.2f GB".format(size / 1e9) else "%.1f MB".format(size / 1e6),
                "Length" to if (durationMs > 0) clock(durationMs) else "—",
                "Storage" to if (file.storage.toInt() == 0) "Internal" else "SD card",
                "Favourite" to if (file.starred) "Yes" else "No",
            )
            rows.forEach { (k, v) ->
                Column {
                    Text(k, style = MaterialTheme.typography.labelMedium, color = scheme.onSurfaceVariant)
                    Text(v, style = MaterialTheme.typography.bodyLarge)
                }
            }
        }
    }
}

@Composable
fun DetailPager(repo: CameraRepository, start: MediaFile, onBack: () -> Unit) {
    val files = remember { repo.browseList.takeIf { l -> l.any { it.path == start.path } } ?: listOf(start) }
    val pager = androidx.compose.foundation.pager.rememberPagerState(initialPage = files.indexOfFirst { it.path == start.path }.coerceAtLeast(0)) { files.size }
    androidx.compose.foundation.pager.HorizontalPager(pager, Modifier.fillMaxSize(), beyondViewportPageCount = 0, key = { files[it].path }) { i ->
        val f = files[i]
        val vm: DetailViewModel = androidx.lifecycle.viewmodel.compose.viewModel(key = f.path) { DetailViewModel(repo, f) }
        DetailScreen(vm, active = pager.currentPage == i, position = "${i + 1} / ${files.size}", onBack = onBack)
    }
}
