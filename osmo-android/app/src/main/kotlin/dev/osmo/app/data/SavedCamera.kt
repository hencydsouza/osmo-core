package dev.osmo.app.data

import android.content.Context
import androidx.core.content.edit

/** The camera the user paired. Stored in app-private prefs; the Wi-Fi password is re-read over BLE on every connect, so it is not stored. */
data class SavedCamera(val address: String, val name: String)

class CameraStore(ctx: Context) {
    private val prefs = ctx.getSharedPreferences("camera", Context.MODE_PRIVATE)

    fun load(): SavedCamera? {
        val a = prefs.getString("address", null) ?: return null
        return SavedCamera(a, prefs.getString("name", null) ?: "Osmo")
    }

    fun save(c: SavedCamera) = prefs.edit { putString("address", c.address).putString("name", c.name) }
    fun forget() = prefs.edit { clear() }
}
