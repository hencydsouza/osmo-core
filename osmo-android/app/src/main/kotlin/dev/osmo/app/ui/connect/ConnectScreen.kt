package dev.osmo.app.ui.connect

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Bluetooth
import androidx.compose.material.icons.filled.CameraAlt
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.LargeTopAppBar
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import dev.osmo.app.data.CameraRepository
import dev.osmo.app.data.FoundCamera
import dev.osmo.app.data.Link
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.launch

class ConnectViewModel(private val repo: CameraRepository) : ViewModel() {
    val link = repo.link
    val saved = repo.saved
    private val _found = MutableStateFlow<List<FoundCamera>>(emptyList())
    val found: StateFlow<List<FoundCamera>> = _found
    private val _scanError = MutableStateFlow<String?>(null)
    val scanError: StateFlow<String?> = _scanError
    private var scan: Job? = null
    private var connecting: Job? = null

    fun startScan() {
        scan?.cancel()
        _scanError.value = null
        scan = viewModelScope.launch {
            repo.scanner.scan().catch { _scanError.value = it.message }.collect { _found.value = it }
        }
    }

    fun stopScan() {
        scan?.cancel()
        scan = null
    }

    fun connect(target: FoundCamera?) {
        stopScan()
        connecting?.cancel()
        connecting = viewModelScope.launch { repo.connect(target) }
    }

    fun cancel() {
        connecting?.cancel()
        repo.disconnect()
    }

    fun forget() = repo.forget()
    fun disconnect() = repo.disconnect()
    override fun onCleared() = stopScan()
}

private fun blePermissions(): Array<String> =
    if (Build.VERSION.SDK_INT >= 31) arrayOf(Manifest.permission.BLUETOOTH_SCAN, Manifest.permission.BLUETOOTH_CONNECT)
    else arrayOf(Manifest.permission.ACCESS_FINE_LOCATION)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ConnectScreen(vm: ConnectViewModel, sdkVersion: String, onConnected: () -> Unit) {
    val link by vm.link.collectAsStateWithLifecycle()
    val saved by vm.saved.collectAsStateWithLifecycle()
    val found by vm.found.collectAsStateWithLifecycle()
    val scanError by vm.scanError.collectAsStateWithLifecycle()
    var denied by remember { mutableStateOf(false) }
    val ctx = LocalContext.current
    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { r ->
        denied = r.values.any { !it }
        if (!denied) vm.startScan()
    }

    fun scan() {
        val missing = blePermissions().filter { ctx.checkSelfPermission(it) != PackageManager.PERMISSION_GRANTED }
        if (missing.isEmpty()) vm.startScan() else launcher.launch(missing.toTypedArray())
    }

    val scroll = TopAppBarDefaults.exitUntilCollapsedScrollBehavior()
    Scaffold(
        modifier = Modifier.nestedScroll(scroll.nestedScrollConnection),
        topBar = { LargeTopAppBar(title = { Text("Osmo") }, scrollBehavior = scroll) },
    ) { pad ->
        LazyColumn(
            Modifier.fillMaxSize().padding(pad),
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            item {
                when (val l = link) {
                    is Link.Connected -> Card(
                        Modifier.fillMaxWidth(),
                        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.primaryContainer),
                    ) {
                        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                                Icon(Icons.Filled.CameraAlt, null)
                                Text("Connected to ${l.name}", style = MaterialTheme.typography.titleMedium)
                            }
                            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                Button(onClick = onConnected) { Text("Open library") }
                                OutlinedButton(onClick = vm::disconnect) { Text("Disconnect") }
                            }
                        }
                    }
                    is Link.Working -> Card(Modifier.fillMaxWidth()) {
                        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                                CircularProgressIndicator()
                                Text(l.step, style = MaterialTheme.typography.titleMedium)
                            }
                            if (l.needsApproval) {
                                Text("Look at the camera screen and confirm pairing.", style = MaterialTheme.typography.bodyMedium)
                            }
                            TextButton(onClick = vm::cancel) { Text("Cancel") }
                        }
                    }
                    else -> Card(Modifier.fillMaxWidth()) {
                        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                            if (l is Link.Failed) Text(l.message, color = MaterialTheme.colorScheme.error)
                            val s = saved
                            if (s != null) {
                                Text(s.name, style = MaterialTheme.typography.titleMedium)
                                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                    Button(onClick = { vm.connect(null) }) { Text("Connect") }
                                    TextButton(onClick = vm::forget) { Text("Forget") }
                                }
                            } else {
                                Text("No camera yet", style = MaterialTheme.typography.titleMedium)
                                Text("Turn the camera on, then scan.", style = MaterialTheme.typography.bodyMedium)
                            }
                        }
                    }
                }
            }
            if (link !is Link.Working && link !is Link.Connected) {
                item {
                    OutlinedButton(onClick = ::scan, contentPadding = ButtonDefaults.ButtonWithIconContentPadding) {
                        Icon(Icons.Filled.Bluetooth, null)
                        Text("Scan for cameras", Modifier.padding(start = 8.dp))
                    }
                }
                if (denied) item { Text("Bluetooth permission is needed to find the camera.", color = MaterialTheme.colorScheme.error) }
                scanError?.let { e -> item { Text(e, color = MaterialTheme.colorScheme.error) } }
                items(found, key = { it.address }) { cam ->
                    ListItem(
                        headlineContent = { Text(cam.name) },
                        supportingContent = { Text("${cam.rssi} dBm") },
                        leadingContent = { Icon(Icons.Filled.CameraAlt, null) },
                        trailingContent = { Button(onClick = { vm.connect(cam) }) { Text("Pair") } },
                    )
                }
            }
            item {
                Text(
                    "SDK $sdkVersion",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}
