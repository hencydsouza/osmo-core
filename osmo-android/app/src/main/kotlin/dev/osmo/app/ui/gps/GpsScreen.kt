@file:OptIn(androidx.compose.material3.ExperimentalMaterial3ExpressiveApi::class, androidx.compose.material3.ExperimentalMaterial3Api::class)

package dev.osmo.app.ui.gps

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.MyLocation
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LoadingIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.osmo.app.data.GpsState
import dev.osmo.app.data.GpsSync

@Composable
fun GpsScreen(gps: GpsSync, onBack: () -> Unit) {
    val ctx = LocalContext.current
    val state by gps.state.collectAsStateWithLifecycle()
    fun granted() = ContextCompat.checkSelfPermission(ctx, Manifest.permission.ACCESS_FINE_LOCATION) == PackageManager.PERMISSION_GRANTED
    var hasLocation by remember { mutableStateOf(granted()) }
    var asked by remember { mutableStateOf(false) }
    val blePerms = if (android.os.Build.VERSION.SDK_INT >= 31) arrayOf(Manifest.permission.BLUETOOTH_CONNECT) else emptyArray()
    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { r ->
        hasLocation = granted()
        asked = true
        if (hasLocation && r.values.all { it }) gps.start()
    }
    val permanentlyDenied = asked && !hasLocation &&
        !androidx.core.app.ActivityCompat.shouldShowRequestPermissionRationale(ctx as Activity, Manifest.permission.ACCESS_FINE_LOCATION)

    Scaffold(topBar = {
        TopAppBar(
            title = { Text("GPS sync", style = MaterialTheme.typography.titleLarge) },
            navigationIcon = { IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
        )
    }) { pad ->
        Column(
            Modifier.padding(pad).fillMaxSize().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp, vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Surface(Modifier.fillMaxWidth(), shape = MaterialTheme.shapes.extraLarge, color = MaterialTheme.colorScheme.surfaceContainerLow) {
                Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text("Put your location in your clips", style = MaterialTheme.typography.titleLarge)
                    Text(
                        "While this is on, your phone sends its GPS position to the camera over Bluetooth about once a second, so what you record is tagged with where you were. The first time, the camera asks you to approve on its screen. Keep the app open while you shoot.",
                        style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            when (val s = state) {
                is GpsState.Running -> {
                    Surface(Modifier.fillMaxWidth(), shape = MaterialTheme.shapes.extraLarge, color = MaterialTheme.colorScheme.primaryContainer) {
                        Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                            LoadingIndicator()
                            Text(s.note, style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.onPrimaryContainer)
                            if (s.sent > 0) Text("${s.sent} fixes sent", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onPrimaryContainer)
                        }
                    }
                    Button(
                        onClick = gps::stop, modifier = Modifier.fillMaxWidth().heightIn(min = 56.dp),
                        colors = androidx.compose.material3.ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.errorContainer, contentColor = MaterialTheme.colorScheme.onErrorContainer),
                    ) { Icon(Icons.Filled.Stop, null); Text("Stop", Modifier.padding(start = 8.dp)) }
                }
                else -> {
                    if (s is GpsState.Failed) {
                        Text(s.message, style = MaterialTheme.typography.bodyLarge, color = MaterialTheme.colorScheme.error)
                    }
                    if (permanentlyDenied) {
                        Text("Location is blocked for Osmo. Turn it on in Settings to use GPS sync.", style = MaterialTheme.typography.bodyMedium)
                        OutlinedButton(onClick = {
                            ctx.startActivity(Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", ctx.packageName, null)))
                        }, Modifier.fillMaxWidth()) { Text("Open settings") }
                    } else {
                        Button(
                            onClick = {
                                if (granted() && blePerms.all { ContextCompat.checkSelfPermission(ctx, it) == PackageManager.PERMISSION_GRANTED }) gps.start()
                                else launcher.launch(arrayOf(Manifest.permission.ACCESS_FINE_LOCATION) + blePerms)
                            },
                            modifier = Modifier.fillMaxWidth().heightIn(min = 64.dp),
                        ) { Icon(Icons.Filled.MyLocation, null); Text("Start GPS sync", Modifier.padding(start = 8.dp), style = MaterialTheme.typography.titleMedium) }
                    }
                }
            }
        }
    }
}
