package dev.osmo.app.ui.detail

import android.graphics.BitmapFactory
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Download
import androidx.compose.material.icons.filled.Star
import androidx.compose.material.icons.filled.StarBorder
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
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
)

class DetailViewModel(private val repo: CameraRepository, val file: MediaFile) : ViewModel() {
    private val _s = MutableStateFlow(DetailState(starred = file.starred))
    val state = _s
    private var job: Job? = null
    suspend fun thumbnail() = repo.thumbnail(file.path)

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

    fun frame(at: Double) = run("Extracting frame") {
        "Saved ${repo.frame(file, at).name}"
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DetailScreen(vm: DetailViewModel, onBack: () -> Unit) {
    val s by vm.state.collectAsStateWithLifecycle()
    val snack = remember { SnackbarHostState() }
    LaunchedEffect(s.message) { s.message?.let { snack.showSnackbar(it) } }
    val thumb by produceState<androidx.compose.ui.graphics.ImageBitmap?>(null) {
        value = vm.thumbnail()?.let { b -> BitmapFactory.decodeByteArray(b, 0, b.size)?.asImageBitmap() }
    }
    var from by remember { mutableStateOf("0") }
    var to by remember { mutableStateOf("5") }
    var at by remember { mutableStateOf("1") }
    val video = vm.file.kind == MediaKind.VIDEO
    val idle = s.busy == null

    Scaffold(
        snackbarHost = { SnackbarHost(snack) },
        topBar = {
            TopAppBar(
                title = { Text(vm.file.path.substringAfterLast('/')) },
                navigationIcon = { IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                actions = {
                    IconButton(onClick = vm::toggleStar, enabled = idle) {
                        Icon(if (s.starred) Icons.Filled.Star else Icons.Filled.StarBorder, if (s.starred) "Remove favourite" else "Favourite")
                    }
                },
            )
        },
    ) { pad ->
        Column(
            Modifier.padding(pad).padding(16.dp).verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            thumb?.let { Image(it, null, Modifier.fillMaxWidth().heightIn(max = 280.dp), contentScale = ContentScale.Fit) }
            Text("${vm.file.sizeBytes.toLong() / 1_000_000} MB · ${if (video) "video" else "photo"}", style = MaterialTheme.typography.bodyMedium)

            s.busy?.let { label ->
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(label, style = MaterialTheme.typography.titleSmall)
                    if (s.total > 0) LinearProgressIndicator(progress = { s.done.toFloat() / s.total }, Modifier.fillMaxWidth())
                    else LinearProgressIndicator(Modifier.fillMaxWidth())
                    TextButton(onClick = vm::cancel) { Text("Cancel") }
                }
            }

            Button(onClick = vm::download, enabled = idle, modifier = Modifier.fillMaxWidth()) {
                Icon(Icons.Filled.Download, null)
                Text("Download", Modifier.padding(start = 8.dp))
            }

            if (video) {
                Text("Trim (no re-encode)", style = MaterialTheme.typography.titleMedium)
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    SecondsField("From (s)", from, { from = it }, Modifier.weight(1f))
                    SecondsField("To (s)", to, { to = it }, Modifier.weight(1f))
                }
                OutlinedButton(
                    onClick = { vm.trim(from.toDoubleOrNull() ?: 0.0, to.toDoubleOrNull() ?: 0.0) },
                    enabled = idle && (from.toDoubleOrNull() ?: -1.0) >= 0 && (to.toDoubleOrNull() ?: 0.0) > (from.toDoubleOrNull() ?: 0.0),
                ) { Text("Save trimmed clip") }

                Text("Still frame", style = MaterialTheme.typography.titleMedium)
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    SecondsField("At (s)", at, { at = it }, Modifier.weight(1f))
                    OutlinedButton(onClick = { vm.frame(at.toDoubleOrNull() ?: 0.0) }, enabled = idle && at.toDoubleOrNull() != null) { Text("Save frame") }
                }
            }
        }
    }
}

@Composable
private fun SecondsField(label: String, value: String, onChange: (String) -> Unit, modifier: Modifier) {
    OutlinedTextField(
        value, onChange, modifier, label = { Text(label) }, singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal),
    )
}
