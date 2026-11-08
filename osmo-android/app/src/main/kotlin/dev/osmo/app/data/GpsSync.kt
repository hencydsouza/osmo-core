package dev.osmo.app.data

import android.annotation.SuppressLint
import android.app.Application
import android.content.Context
import android.location.Location
import android.location.LocationListener
import android.location.LocationManager
import android.os.Looper
import androidx.core.content.edit
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import uniffi.osmo_ffi.GpsIdentity
import uniffi.osmo_ffi.GpsPoint
import uniffi.osmo_ffi.GpsSource
import uniffi.osmo_ffi.newGpsIdentity
import uniffi.osmo_ffi.runGpsSync

sealed interface GpsState {
    data object Idle : GpsState
    data class Running(val note: String, val sent: Int) : GpsState
    data class Failed(val message: String) : GpsState
}

/**
 * Feeds the phone's location to the camera over BLE (R-SDK) so clips get GPS. Runs while the app is open;
 * the camera may ask for approval on its screen the first time.
 */
class GpsSync(private val app: Application, private val repo: CameraRepository) {
    private val prefs = app.getSharedPreferences("gps", Context.MODE_PRIVATE)
    private val _state = MutableStateFlow<GpsState>(GpsState.Idle)
    val state: StateFlow<GpsState> = _state.asStateFlow()
    @Volatile private var stopFlag = false
    @Volatile private var thread: Thread? = null

    private fun identity(): GpsIdentity {
        val id = prefs.getLong("id", 0L).toUInt()
        val mac = prefs.getString("mac", null)
        if (id != 0u && mac != null && mac.length == 12) {
            return GpsIdentity(id, ByteArray(6) { mac.substring(it * 2, it * 2 + 2).toInt(16).toByte() })
        }
        val fresh = newGpsIdentity()
        prefs.edit {
            putLong("id", fresh.deviceId.toLong())
            putString("mac", fresh.mac.joinToString("") { "%02x".format(it) })
        }
        return fresh
    }

    val running get() = thread?.isAlive == true

    @SuppressLint("MissingPermission")
    fun start() {
        if (running) return
        val device = repo.savedDevice() ?: run { _state.value = GpsState.Failed("No camera saved. Connect once first."); return }
        stopFlag = false
        _state.value = GpsState.Running("Connecting over Bluetooth", 0)
        thread = Thread({
            val lm = app.getSystemService(Context.LOCATION_SERVICE) as LocationManager
            val last = java.util.concurrent.atomic.AtomicReference<Location?>(null)
            var sent = 0
            val listener = LocationListener { last.set(it) }
            try {
                lm.requestLocationUpdates(LocationManager.GPS_PROVIDER, 1000L, 0f, listener, Looper.getMainLooper())
                val source = object : GpsSource {
                    override fun latest(): GpsPoint? = last.get()?.let {
                        GpsPoint(
                            observedUnixMs = it.time.toULong(),
                            latitude = it.latitude, longitude = it.longitude,
                            altitudeM = if (it.hasAltitude()) it.altitude else 0.0,
                            speedMps = if (it.hasSpeed()) it.speed.toDouble() else 0.0,
                            bearingDeg = if (it.hasBearing()) it.bearing.toDouble() else 0.0,
                            horizontalAccuracyM = if (it.hasAccuracy()) it.accuracy.toDouble() else 10.0,
                            verticalAccuracyM = if (it.hasVerticalAccuracy()) it.verticalAccuracyMeters.toDouble() else 10.0,
                            speedAccuracyMps = if (it.hasSpeedAccuracy()) it.speedAccuracyMetersPerSecond.toDouble() else 1.0,
                            satellites = it.extras?.getInt("satellites", 0)?.toUInt() ?: 0u,
                        )
                    }
                    override fun stopped() = stopFlag
                    override fun onEvent(message: String) {
                        if (message.startsWith("sent ")) sent = message.removePrefix("sent ").substringBefore(' ').toIntOrNull() ?: sent
                        _state.value = GpsState.Running(if (message.startsWith("sent")) "Sending location to the camera" else message, sent)
                    }
                }
                GattLink(app, device, armPairing = false).use { link ->
                    link.open()
                    _state.value = GpsState.Running("Waiting for approval on the camera", 0)
                    runGpsSync(link, identity(), source)
                }
                _state.value = GpsState.Idle
            } catch (e: Throwable) {
                _state.value = if (stopFlag) GpsState.Idle else GpsState.Failed(e.friendly())
            } finally {
                runCatching { lm.removeUpdates(listener) }
            }
        }, "osmo-gps").also { it.start() }
    }

    fun stop() { stopFlag = true }
}
