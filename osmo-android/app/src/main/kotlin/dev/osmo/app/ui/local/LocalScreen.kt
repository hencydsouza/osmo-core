@file:OptIn(androidx.compose.material3.ExperimentalMaterial3ExpressiveApi::class)

package dev.osmo.app.ui.local

import android.content.Intent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.OpenInNew
import androidx.compose.material.icons.filled.ContentCut
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Share
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearWavyProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
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
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import dev.osmo.app.data.CameraRepository
import dev.osmo.app.data.friendly
import java.io.File
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class LocalState(
    val files: List<File> = emptyList(),
    val busy: String? = null,
    val done: Long = 0,
    val total: Long = 0,
    val message: String? = null,
)

/** Files this app saved on the phone. Trimming here uses the SDK's local trim, so no camera is needed. */
class LocalViewModel(private val repo: CameraRepository) : ViewModel() {
    private val _s = MutableStateFlow(LocalState())
    val state = _s

    fun refresh() {
        viewModelScope.launch { _s.update { it.copy(files = repo.localFiles()) } }
    }

    fun uri(f: File) = repo.uriFor(f)

    fun delete(f: File) {
        viewModelScope.launch { repo.deleteLocal(f); refresh() }
    }

    fun trim(f: File, from: Double, to: Double) {
        if (_s.value.busy != null) return
        _s.update { it.copy(busy = "Trimming ${f.name}", done = 0, total = 0, message = null) }
        viewModelScope.launch {
            runCatching { repo.trimLocal(f, from, to) { d, t -> _s.update { it.copy(done = d, total = t) } } }
                .onSuccess { (out, r) -> _s.update { it.copy(busy = null, message = "Saved ${out.name}: ${"%.1f".format(r.durationSecs)}s") }; refresh() }
                .onFailure { e -> _s.update { it.copy(busy = null, message = e.friendly()) } }
        }
    }

    fun cancel() = repo.cancel()
    fun consumeMessage() = _s.update { it.copy(message = null) }
}

private fun File.isVideo() = extension.equals("mp4", true) || extension.equals("mov", true)

private fun mime(f: File) = when (f.extension.lowercase()) {
    "mp4", "mov" -> "video/mp4"
    "jpg", "jpeg" -> "image/jpeg"
    "dng" -> "image/x-adobe-dng"
    else -> "*/*"
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LocalScreen(vm: LocalViewModel, onBack: () -> Unit) {
    val s by vm.state.collectAsStateWithLifecycle()
    val ctx = LocalContext.current
    val snack = remember { SnackbarHostState() }
    var trimming by remember { mutableStateOf<File?>(null) }
    LaunchedEffect(Unit) { vm.refresh() }
    LaunchedEffect(s.message) { s.message?.let { snack.showSnackbar(it); vm.consumeMessage() } }

    Scaffold(
        snackbarHost = { SnackbarHost(snack) },
        topBar = {
            TopAppBar(
                title = { Text("On this phone") },
                navigationIcon = { IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
            )
        },
    ) { pad ->
        Column(Modifier.padding(pad).fillMaxSize()) {
            s.busy?.let { label ->
                Column(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) {
                    Text(label, style = MaterialTheme.typography.titleSmall)
                    if (s.total > 0) LinearWavyProgressIndicator(progress = { s.done.toFloat() / s.total }, Modifier.fillMaxWidth())
                    else LinearWavyProgressIndicator(Modifier.fillMaxWidth())
                    TextButton(onClick = vm::cancel) { Text("Cancel") }
                }
            }
            if (s.files.isEmpty()) {
                Text("Nothing saved yet. Download from the camera first.", Modifier.padding(16.dp).align(Alignment.CenterHorizontally))
            }
            LazyColumn(Modifier.fillMaxSize()) {
                items(s.files, key = { it.path }) { f ->
                    ListItem(
                        headlineContent = { Text(f.name, maxLines = 1) },
                        supportingContent = { Text("${f.length() / 1_000_000} MB") },
                        trailingContent = {
                            Row {
                                if (f.isVideo()) IconButton(onClick = { trimming = f }, enabled = s.busy == null) { Icon(Icons.Filled.ContentCut, "Trim") }
                                IconButton(onClick = {
                                    val i = Intent(Intent.ACTION_VIEW).setDataAndType(vm.uri(f), mime(f)).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                                    runCatching { ctx.startActivity(i) }
                                }) { Icon(Icons.AutoMirrored.Filled.OpenInNew, "Open") }
                                IconButton(onClick = {
                                    val i = Intent(Intent.ACTION_SEND).setType(mime(f)).putExtra(Intent.EXTRA_STREAM, vm.uri(f)).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                                    ctx.startActivity(Intent.createChooser(i, "Share"))
                                }) { Icon(Icons.Filled.Share, "Share") }
                                IconButton(onClick = { vm.delete(f) }) { Icon(Icons.Filled.Delete, "Delete from phone") }
                            }
                        },
                    )
                }
            }
        }
    }

    trimming?.let { f ->
        var from by remember { mutableStateOf("0") }
        var to by remember { mutableStateOf("5") }
        val a = from.toDoubleOrNull()
        val b = to.toDoubleOrNull()
        AlertDialog(
            onDismissRequest = { trimming = null },
            title = { Text("Trim ${f.name}") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedTextField(from, { from = it }, label = { Text("From (s)") }, singleLine = true, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal))
                    OutlinedTextField(to, { to = it }, label = { Text("To (s)") }, singleLine = true, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal))
                }
            },
            confirmButton = {
                TextButton(enabled = a != null && b != null && a >= 0 && b > a, onClick = { vm.trim(f, a!!, b!!); trimming = null }) { Text("Trim") }
            },
            dismissButton = { TextButton(onClick = { trimming = null }) { Text("Cancel") } },
        )
    }
}
