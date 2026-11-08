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
import androidx.compose.material.icons.filled.Star
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
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
)

class LibraryViewModel(private val repo: CameraRepository) : ViewModel() {
    private val _s = MutableStateFlow(LibraryState())
    val state = _s
    suspend fun thumbnail(path: String) = repo.thumbnail(path)

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

@OptIn(ExperimentalMaterial3Api::class, ExperimentalFoundationApi::class)
@Composable
fun LibraryScreen(vm: LibraryViewModel, onOpen: (MediaFile) -> Unit, onBack: () -> Unit) {
    val s by vm.state.collectAsStateWithLifecycle()
    val snack = remember { SnackbarHostState() }
    LaunchedEffect(Unit) { if (vm.state.value.files.isEmpty()) vm.refresh() }
    LaunchedEffect(s.message) { s.message?.let { snack.showSnackbar(it); vm.consumeMessage() } }

    Scaffold(
        snackbarHost = { SnackbarHost(snack) },
        topBar = {
            if (s.selected.isEmpty()) {
                TopAppBar(
                    title = { Text("Library") },
                    navigationIcon = { IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                    actions = { IconButton(onClick = vm::refresh) { Icon(Icons.Filled.Refresh, "Refresh") } },
                )
            } else {
                TopAppBar(
                    title = { Text("${s.selected.size} selected") },
                    navigationIcon = { IconButton(onClick = vm::clearSelection) { Icon(Icons.Filled.Close, "Clear selection") } },
                    actions = { IconButton(onClick = vm::planDelete) { Icon(Icons.Filled.Delete, "Delete") } },
                )
            }
        },
    ) { pad ->
        Column(Modifier.padding(pad).fillMaxSize()) {
            androidx.compose.foundation.layout.Row(
                Modifier.padding(horizontal = 16.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                FilterChip(selected = s.kind == MediaKind.VIDEO, onClick = { vm.setKind(if (s.kind == MediaKind.VIDEO) null else MediaKind.VIDEO) }, label = { Text("Videos") })
                FilterChip(selected = s.kind == MediaKind.PHOTO, onClick = { vm.setKind(if (s.kind == MediaKind.PHOTO) null else MediaKind.PHOTO) }, label = { Text("Photos") })
                FilterChip(selected = s.starredOnly, onClick = vm::toggleStarred, label = { Text("Starred") })
            }
            Box(Modifier.fillMaxSize()) {
                if (s.loading && s.files.isEmpty()) CircularProgressIndicator(Modifier.align(Alignment.Center))
                else if (s.files.isEmpty()) Text("Nothing on the camera.", Modifier.align(Alignment.Center))
                LazyVerticalGrid(
                    columns = GridCells.Adaptive(112.dp),
                    contentPadding = PaddingValues(16.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                    modifier = Modifier.fillMaxSize(),
                ) {
                    items(s.files, key = { it.path }) { f ->
                        Tile(f, f.path in s.selected, vm,
                            onClick = { if (s.selected.isEmpty()) onOpen(f) else vm.toggle(f.path) },
                            onLong = { vm.toggle(f.path) })
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
        value = vm.thumbnail(f.path)?.let { b -> BitmapFactory.decodeByteArray(b, 0, b.size)?.asImageBitmap() }
    }
    Box(
        Modifier.aspectRatio(1f).clip(MaterialTheme.shapes.medium)
            .background(MaterialTheme.colorScheme.surfaceContainerHigh)
            .combinedClickable(onClick = onClick, onLongClick = onLong),
    ) {
        bmp?.let { Image(it, f.path, Modifier.fillMaxSize(), contentScale = ContentScale.Crop) }
        if (f.kind == MediaKind.VIDEO) Icon(Icons.Filled.PlayCircle, null, Modifier.align(Alignment.Center), tint = MaterialTheme.colorScheme.surface)
        if (f.starred) Icon(Icons.Filled.Star, "Starred", Modifier.align(Alignment.TopEnd).padding(6.dp), tint = MaterialTheme.colorScheme.tertiary)
        if (selected) {
            Box(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.primary.copy(alpha = 0.35f)))
            Icon(Icons.Filled.CheckCircle, "Selected", Modifier.align(Alignment.TopStart).padding(6.dp), tint = MaterialTheme.colorScheme.onPrimary)
        }
    }
}
