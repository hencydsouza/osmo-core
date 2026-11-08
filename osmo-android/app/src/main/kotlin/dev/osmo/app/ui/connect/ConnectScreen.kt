@file:OptIn(androidx.compose.material3.ExperimentalMaterial3ExpressiveApi::class)

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
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material.icons.filled.PhoneAndroid
import androidx.compose.material3.LargeTopAppBar
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.material.icons.filled.Bolt
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.PhotoLibrary
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.IconButton
import androidx.compose.material3.LoadingIndicator
import androidx.compose.material3.MaterialShapes
import androidx.compose.material3.Surface
import androidx.compose.material3.toShape
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.text.style.TextAlign
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

@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun ConnectScreen(vm: ConnectViewModel, sdkVersion: String, onLocal: () -> Unit, onConnected: () -> Unit) {
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

    val working = link is Link.Working
    val connected = link is Link.Connected
    val scheme = MaterialTheme.colorScheme

    Scaffold(
        containerColor = scheme.surface,
        topBar = {
            Row(Modifier.fillMaxWidth().statusBarsPadding().padding(horizontal = 8.dp, vertical = 4.dp), horizontalArrangement = Arrangement.End) {
                IconButton(onClick = onLocal) { Icon(Icons.Filled.PhoneAndroid, "On this phone") }
            }
        },
    ) { pad ->
        LazyColumn(
            Modifier.fillMaxSize().padding(pad),
            contentPadding = PaddingValues(horizontal = 24.dp, vertical = 8.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            // Hero moment (the one per product): the camera badge. A 9-sided cookie that spins while
            // pairing and settles when connected. Shape from the M3 shape library.
            item {
                Box(Modifier.padding(top = 16.dp, bottom = 28.dp), contentAlignment = Alignment.Center) {
                    val spin by rememberInfiniteTransition(label = "spin").animateFloat(
                        0f, 360f, infiniteRepeatable(tween(9000, easing = LinearEasing)), label = "deg",
                    )
                    Box(
                        Modifier.size(176.dp).rotate(if (working) spin else 0f)
                            .background(if (connected) scheme.secondaryContainer else scheme.primaryContainer, MaterialShapes.Cookie9Sided.toShape()),
                    )
                    if (working) LoadingIndicator(Modifier.size(72.dp), color = scheme.onPrimaryContainer)
                    else Icon(
                        if (connected) Icons.Filled.CheckCircle else Icons.Filled.CameraAlt, null,
                        Modifier.size(72.dp), tint = if (connected) scheme.onSecondaryContainer else scheme.onPrimaryContainer,
                    )
                }
            }
            item {
                val (title, sub) = when (val l = link) {
                    is Link.Connected -> "Connected" to l.name
                    is Link.Working -> l.step to (if (l.needsApproval) "Look at the camera screen and confirm pairing." else "Hang on, this takes a few seconds.")
                    is Link.Failed -> "Couldn\u2019t connect" to l.message
                    else -> (if (saved != null) "Welcome back" else "Find your Osmo") to
                        (saved?.name ?: "Turn the camera on, then scan.")
                }
                Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(title, style = MaterialTheme.typography.headlineLarge, textAlign = TextAlign.Center)
                    Text(
                        sub, style = MaterialTheme.typography.bodyLarge, textAlign = TextAlign.Center,
                        color = if (link is Link.Failed) scheme.error else scheme.onSurfaceVariant,
                    )
                }
            }
            item {
                Column(Modifier.padding(top = 32.dp).fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    when (link) {
                        is Link.Connected -> {
                            // Primary action is the biggest thing on screen.
                            Button(onClick = onConnected, Modifier.fillMaxWidth().heightIn(min = 64.dp), shape = MaterialTheme.shapes.extraLarge) {
                                Icon(Icons.Filled.PhotoLibrary, null)
                                Text("Open library", Modifier.padding(start = 12.dp), style = MaterialTheme.typography.titleMedium)
                            }
                            TextButton(onClick = vm::disconnect, Modifier.fillMaxWidth()) { Text("Disconnect") }
                        }
                        is Link.Working -> TextButton(onClick = vm::cancel, Modifier.fillMaxWidth()) { Text("Cancel") }
                        else -> {
                            if (saved != null) {
                                Button(onClick = { vm.connect(null) }, Modifier.fillMaxWidth().heightIn(min = 64.dp), shape = MaterialTheme.shapes.extraLarge) {
                                    Icon(Icons.Filled.Bolt, null)
                                    Text("Connect to ${saved!!.name}", Modifier.padding(start = 12.dp), style = MaterialTheme.typography.titleMedium, maxLines = 1)
                                }
                            }
                            val scanColors = if (saved != null) null else ButtonDefaults.buttonColors()
                            if (scanColors != null) {
                                Button(onClick = ::scan, Modifier.fillMaxWidth().heightIn(min = 64.dp), shape = MaterialTheme.shapes.extraLarge) {
                                    Icon(Icons.Filled.Bluetooth, null)
                                    Text("Scan for cameras", Modifier.padding(start = 12.dp), style = MaterialTheme.typography.titleMedium)
                                }
                            } else {
                                androidx.compose.material3.OutlinedButton(onClick = ::scan, Modifier.fillMaxWidth().heightIn(min = 56.dp)) {
                                    Icon(Icons.Filled.Bluetooth, null)
                                    Text("Scan for another camera", Modifier.padding(start = 8.dp))
                                }
                                TextButton(onClick = vm::forget, Modifier.fillMaxWidth()) { Text("Forget this camera") }
                            }
                            if (denied) Text("Bluetooth permission is needed to find the camera.", color = scheme.error, style = MaterialTheme.typography.bodyMedium)
                            scanError?.let { Text(it, color = scheme.error, style = MaterialTheme.typography.bodyMedium) }
                        }
                    }
                }
            }
            if (!working && !connected && found.isNotEmpty()) {
                item { Text("Nearby", Modifier.fillMaxWidth().padding(top = 28.dp, bottom = 8.dp), style = MaterialTheme.typography.titleMedium) }
                items(found, key = { it.address }) { cam ->
                    Surface(
                        onClick = { vm.connect(cam) },
                        Modifier.fillMaxWidth().padding(bottom = 8.dp),
                        shape = MaterialTheme.shapes.large,
                        color = scheme.surfaceContainerHigh,
                    ) {
                        Row(Modifier.padding(16.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                            Box(Modifier.size(44.dp).background(scheme.primaryContainer, MaterialShapes.Cookie6Sided.toShape()), contentAlignment = Alignment.Center) {
                                Icon(Icons.Filled.CameraAlt, null, Modifier.size(22.dp), tint = scheme.onPrimaryContainer)
                            }
                            Column(Modifier.weight(1f)) {
                                Text(cam.name, style = MaterialTheme.typography.titleMedium)
                                Text("${cam.rssi} dBm", style = MaterialTheme.typography.bodyMedium, color = scheme.onSurfaceVariant)
                            }
                            Text("Pair", style = MaterialTheme.typography.labelLarge, color = scheme.primary)
                        }
                    }
                }
            }
            item {
                Text(
                    "SDK $sdkVersion", Modifier.padding(top = 32.dp, bottom = 16.dp),
                    style = MaterialTheme.typography.labelMedium, color = scheme.onSurfaceVariant,
                )
            }
        }
    }
}
