package dev.osmo.app.data

import android.annotation.SuppressLint
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothManager
import android.bluetooth.le.ScanCallback
import android.bluetooth.le.ScanResult
import android.bluetooth.le.ScanSettings
import android.content.Context
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow

data class FoundCamera(val device: BluetoothDevice, val name: String, val rssi: Int) {
    val address: String get() = device.address
}

/** DJI manufacturer ids as Android reports them (see osmo-core/src/ble/advert.rs). */
private val DJI_COMPANY_IDS = intArrayOf(0x08AA, 0xF7AA, 0xE5C0)

fun looksLikeOsmo(result: ScanResult): Boolean {
    val rec = result.scanRecord
    val dji = rec?.manufacturerSpecificData?.let { m -> DJI_COMPANY_IDS.any { m.indexOfKey(it) >= 0 } } == true
    val name = rec?.deviceName.orEmpty()
    return dji || name.startsWith("Osmo", ignoreCase = true)
}

class BleScanner(private val ctx: Context) {
    val adapter get() = ctx.getSystemService(BluetoothManager::class.java)?.adapter

    /** Emits the growing list of cameras in range. Needs BLUETOOTH_SCAN (API 31+) or location (API <= 30). */
    @SuppressLint("MissingPermission")
    fun scan(): Flow<List<FoundCamera>> = callbackFlow {
        val scanner = adapter?.bluetoothLeScanner
        if (scanner == null) {
            close(IllegalStateException("Bluetooth is off or unavailable"))
            return@callbackFlow
        }
        val seen = LinkedHashMap<String, FoundCamera>()
        val cb = object : ScanCallback() {
            override fun onScanResult(callbackType: Int, result: ScanResult) {
                if (!looksLikeOsmo(result)) return
                val name = result.scanRecord?.deviceName ?: result.device.name ?: "Osmo camera"
                seen[result.device.address] = FoundCamera(result.device, name, result.rssi)
                trySend(seen.values.sortedByDescending { it.rssi })
            }

            override fun onScanFailed(errorCode: Int) {
                close(IllegalStateException("Bluetooth scan failed ($errorCode)"))
            }
        }
        scanner.startScan(null, ScanSettings.Builder().setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY).build(), cb)
        awaitClose { runCatching { scanner.stopScan(cb) } }
    }
}
