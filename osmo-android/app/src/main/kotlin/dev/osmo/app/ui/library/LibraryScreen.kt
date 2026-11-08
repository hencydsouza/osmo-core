@file:OptIn(androidx.compose.material3.ExperimentalMaterial3ExpressiveApi::class)

package dev.osmo.app.ui.library

import android.graphics.BitmapFactory
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.PlayCircle
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.PhoneAndroid
import androidx.compose.material.icons.filled.Download
import androidx.compose.material.icons.filled.StarBorder
import androidx.compose.material.icons.filled.Star
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.layout.size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import dev.osmo.app.data.CameraRepository
import dev.osmo.app.data.friendly
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.osmo_ffi.MediaFile
import uniffi.osmo_ffi.MediaKind

data class LibraryState(
    val files: List<MediaFile> = emptyList(),
    val loading: Boolean = false,
    val kind: MediaKind? = null,
    val starredOnly: Boolean = false,
    val selected: Set<String> = emptySet(),
    /** Files a pending delete would remove (from the SDK's plan; nothing is sent until confirmed). */
    val deletePlan: List<MediaFile>? = null,
    val message: String? = null,
    /** Bulk operation in flight: label, and (file index, file count, bytes done, bytes total) for downloads. */
    val busy: String? = null,
    val bulk: BulkProgress? = null,
)

data class BulkProgress(val index: Int, val count: Int, val done: Long, val total: Long)

class LibraryViewModel(private val repo: CameraRepository) : ViewModel() {
    private val _s = MutableStateFlow(LibraryState())
    val state = _s
    suspend fun thumbnail(f: MediaFile) = repo.thumbnail(f)
    suspend fun status() = runCatching { repo.status() }.getOrNull()

    fun refresh() {
        _s.update { it.copy(loading = true, message = null) }
        viewModelScope.launch {
            val s = _s.value
            runCatching { repo.list(s.kind, s.starredOnly) }
                .onSuccess { f -> _s.update { it.copy(files = f, loading = false, selected = it.selected.filter { p -> f.any { x -> x.path == p } }.toSet()) } }
                .onFailure { e -> _s.update { it.copy(loading = false, message = e.friendly()) } }
        }
    }

    fun setKind(k: MediaKind?) { _s.update { it.copy(kind = k) }; refresh() }
    fun toggleStarred() { _s.update { it.copy(starredOnly = !it.starredOnly) }; refresh() }
    fun toggle(path: String) = _s.update { it.copy(selected = if (path in it.selected) it.selected - path else it.selected + path) }
    fun clearSelection() = _s.update { it.copy(selected = emptySet()) }
    fun consumeMessage() = _s.update { it.copy(message = null) }

    fun planDelete() {
        val paths = _s.value.selected.toList()
        viewModelScope.launch {
            runCatching { repo.planDelete(paths) }
                .onSuccess { p -> _s.update { it.copy(deletePlan = p) } }
                .onFailure { e -> _s.update { it.copy(message = e.friendly()) } }
        }
    }

    fun cancelDelete() = _s.update { it.copy(deletePlan = null) }

    fun cancelBulk() = repo.cancel()

    fun starSelected(on: Boolean) {
        val paths = _s.value.selected.toList()
        if (_s.value.busy != null) return
        _s.update { it.copy(busy = if (on) "Starring" else "Un-starring") }
        viewModelScope.launch {
            runCatching { repo.setFavouriteMany(paths, on) }
                .onSuccess { n -> _s.update { it.copy(busy = null, selected = emptySet(), message = "${if (on) "Starred" else "Un-starred"} $n of ${paths.size}") }; refresh() }
                .onFailure { e -> _s.update { it.copy(busy = null, message = e.friendly()) } }
        }
    }

    fun downloadSelected() {
        val s = _s.value
        if (s.busy != null) return
        val files = s.files.filter { it.path in s.selected }
        _s.update { it.copy(busy = "Downloading", bulk = BulkProgress(0, files.size, 0, 0)) }
        viewModelScope.launch {
            runCatching { repo.downloadMany(files) { i, n, d, t -> _s.update { it.copy(bulk = BulkProgress(i, n, d, t)) } } }
                .onSuccess { r -> _s.update { it.copy(busy = null, bulk = null, selected = emptySet(), message = "Saved ${r.size} file${if (r.size == 1) "" else "s"} to Movies/Pictures › Osmo") } }
                .onFailure { e -> _s.update { it.copy(busy = null, bulk = null, message = e.friendly()) } }
        }
    }

    fun confirmDelete() {
        val paths = _s.value.deletePlan?.map { it.path } ?: return
        _s.update { it.copy(deletePlan = null, loading = true) }
        viewModelScope.launch {
            runCatching { repo.delete(paths) }
                .onSuccess { _s.update { it.copy(selected = emptySet(), message = "Deleted ${paths.size}") }; refresh() }
                .onFailure { e -> _s.update { it.copy(loading = false, message = e.friendly()) } }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class, ExperimentalFoundationApi::class, androidx.compose.material3.ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun LibraryScreen(vm: LibraryViewModel, onOpen: (MediaFile) -> Unit, onLocal: () -> Unit, onBack: () -> Unit) {
    val s by vm.state.collectAsStateWithLifecycle()
    val snack = remember { SnackbarHostState() }
    val haptic = androidx.compose.ui.platform.LocalHapticFeedback.current
    LaunchedEffect(Unit) { if (vm.state.value.files.isEmpty()) vm.refresh() }
    var cam by remember { androidx.compose.runtime.mutableStateOf<uniffi.osmo_ffi.CameraStatus?>(null) }
    LaunchedEffect(Unit) { while (true) { vm.status()?.let { cam = it }; kotlinx.coroutines.delay(5000) } }
    LaunchedEffect(s.message) { s.message?.let { snack.showSnackbar(it); vm.consumeMessage() } }

    Scaffold(
        snackbarHost = { SnackbarHost(snack) },
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text(if (s.selected.isEmpty()) "Library" else "${s.selected.size} selected", style = MaterialTheme.typography.titleLarge)
                        if (s.selected.isEmpty() && s.files.isNotEmpty()) {
                            val c = cam
                            val parts = buildList {
                                add("${s.files.size} on camera")
                                c?.batteryPercent?.let { add("battery $it%${if (c.charging == true) " ⚡" else ""}") }
                                val free = c?.sdFreeMb ?: c?.internalFreeMb
                                free?.let { add("${"%.1f".format(it.toDouble() / 1024)} GB free") }
                            }
                            Text(parts.joinToString(" · "), style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                },
                navigationIcon = {
                    if (s.selected.isEmpty()) IconButton(onClick = onBack, colors = IconButtonDefaults.iconButtonColors(containerColor = MaterialTheme.colorScheme.surfaceBright)) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") }
                    else IconButton(onClick = vm::clearSelection) { Icon(Icons.Filled.Close, "Clear selection") }
                },
                actions = {
                    if (s.selected.isEmpty()) {
                        IconButton(onClick = onLocal) { Icon(Icons.Filled.PhoneAndroid, "On this phone") }
                        IconButton(onClick = vm::refresh) { Icon(Icons.Filled.Refresh, "Refresh") }
                    }
                },
            )
        },
    ) { pad ->
        Column(Modifier.padding(pad).fillMaxSize()) {
            s.busy?.let { label ->
                Column(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) {
                    val b = s.bulk
                    Text(if (b != null) "$label ${b.index + 1} of ${b.count}" else label, style = MaterialTheme.typography.titleSmall)
                    if (b != null && b.total > 0) androidx.compose.material3.LinearWavyProgressIndicator(progress = { b.done.toFloat() / b.total }, Modifier.fillMaxWidth())
                    else androidx.compose.material3.LinearWavyProgressIndicator(Modifier.fillMaxWidth())
                    TextButton(onClick = vm::cancelBulk) { Text("Cancel") }
                }
            }
            androidx.compose.foundation.layout.Row(
                Modifier.padding(horizontal = 16.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                FilterChip(selected = s.kind == MediaKind.VIDEO, onClick = { vm.setKind(if (s.kind == MediaKind.VIDEO) null else MediaKind.VIDEO) }, label = { Text("Videos") })
                FilterChip(selected = s.kind == MediaKind.PHOTO, onClick = { vm.setKind(if (s.kind == MediaKind.PHOTO) null else MediaKind.PHOTO) }, label = { Text("Photos") })
                FilterChip(selected = s.starredOnly, onClick = vm::toggleStarred, label = { Text("Starred") })
            }
            Box(Modifier.fillMaxSize()) {
                if (s.loading && s.files.isEmpty()) androidx.compose.material3.LoadingIndicator(Modifier.align(Alignment.Center).size(64.dp))
                else if (s.files.isEmpty()) Text("Nothing on the camera yet.", Modifier.align(Alignment.Center), style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                LazyVerticalGrid(
                    columns = GridCells.Adaptive(112.dp),
                    contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 12.dp, bottom = 112.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                    modifier = Modifier.fillMaxSize(),
                ) {
                    items(s.files, key = { it.path }) { f ->
                        Tile(f, f.path in s.selected, vm,
                            onClick = { if (s.selected.isEmpty()) onOpen(f) else vm.toggle(f.path) },
                            onLong = { haptic.performHapticFeedback(androidx.compose.ui.hapticfeedback.HapticFeedbackType.LongPress); vm.toggle(f.path) })
                    }
                }
                androidx.compose.animation.AnimatedVisibility(
                    visible = s.selected.isNotEmpty(),
                    modifier = Modifier.align(Alignment.BottomCenter).padding(bottom = 16.dp),
                    enter = androidx.compose.animation.fadeIn() + androidx.compose.animation.slideInVertically { it },
                    exit = androidx.compose.animation.fadeOut() + androidx.compose.animation.slideOutVertically { it },
                ) {
                    val idle = s.busy == null
                    androidx.compose.material3.HorizontalFloatingToolbar(expanded = true) {
                        IconButton(onClick = vm::downloadSelected, enabled = idle) { Icon(Icons.Filled.Download, "Download selected") }
                        IconButton(onClick = { vm.starSelected(true) }, enabled = idle) { Icon(Icons.Filled.Star, "Star selected") }
                        IconButton(onClick = { vm.starSelected(false) }, enabled = idle) { Icon(Icons.Filled.StarBorder, "Un-star selected") }
                        IconButton(onClick = vm::planDelete, enabled = idle) { Icon(Icons.Filled.Delete, "Delete selected", tint = MaterialTheme.colorScheme.error) }
                    }
                }
            }
        }
    }

    s.deletePlan?.let { plan ->
        AlertDialog(
            onDismissRequest = vm::cancelDelete,
            title = { Text("Delete ${plan.size} file${if (plan.size == 1) "" else "s"} from the camera?") },
            text = { Text("This cannot be undone. Download anything you want to keep first.\n\n" + plan.take(5).joinToString("\n") { it.path.substringAfterLast('/') } + if (plan.size > 5) "\n…and ${plan.size - 5} more" else "") },
            confirmButton = { TextButton(onClick = vm::confirmDelete) { Text("Delete", color = MaterialTheme.colorScheme.error) } },
            dismissButton = { TextButton(onClick = vm::cancelDelete) { Text("Keep") } },
        )
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun Tile(f: MediaFile, selected: Boolean, vm: LibraryViewModel, onClick: () -> Unit, onLong: () -> Unit) {
    val bmp by produceState<androidx.compose.ui.graphics.ImageBitmap?>(null, f.path) {
        value = vm.thumbnail(f)?.let { b -> BitmapFactory.decodeByteArray(b, 0, b.size)?.asImageBitmap() }
    }
    val scheme = MaterialTheme.colorScheme
    // Selection morphs the tile: corners grow and it shrinks inside its cell (spatial spring, expressive scheme).
    // Radii are scale steps from the M3 shape scale (medium 12dp -> extra-large 28dp).
    val spec = MaterialTheme.motionScheme.defaultSpatialSpec<androidx.compose.ui.unit.Dp>()
    val radius by androidx.compose.animation.core.animateDpAsState(if (selected) 28.dp else 12.dp, spec, label = "radius")
    val scaleSpec = MaterialTheme.motionScheme.defaultSpatialSpec<Float>()
    val scale by androidx.compose.animation.core.animateFloatAsState(if (selected) 0.9f else 1f, scaleSpec, label = "scale")
    Box(
        Modifier.aspectRatio(1f).graphicsLayer { scaleX = scale; scaleY = scale }
            .clip(androidx.compose.foundation.shape.RoundedCornerShape(radius))
            .background(scheme.surfaceContainerHigh)
            .combinedClickable(onClick = onClick, onLongClick = onLong),
    ) {
        bmp?.let { Image(it, f.path, Modifier.fillMaxSize(), contentScale = ContentScale.Crop) }
        if (f.kind == MediaKind.VIDEO) {
            Box(Modifier.align(Alignment.BottomStart).padding(6.dp).background(scheme.scrim.copy(alpha = 0.55f), CircleShape).padding(4.dp)) {
                Icon(Icons.Filled.PlayArrow, "Video", Modifier.size(16.dp), tint = Color.White)
            }
        }
        if (f.starred) {
            Box(Modifier.align(Alignment.TopEnd).padding(6.dp).background(scheme.tertiaryContainer, CircleShape).padding(4.dp)) {
                Icon(Icons.Filled.Star, "Starred", Modifier.size(14.dp), tint = scheme.onTertiaryContainer)
            }
        }
        if (selected) {
            Box(Modifier.fillMaxSize().background(scheme.primary.copy(alpha = 0.28f)))
            Box(Modifier.align(Alignment.TopStart).padding(8.dp).background(scheme.primary, CircleShape).padding(3.dp)) {
                Icon(Icons.Filled.Check, "Selected", Modifier.size(16.dp), tint = scheme.onPrimary)
            }
        }
    }
}
