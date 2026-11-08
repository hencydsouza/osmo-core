package dev.osmo.app

import android.app.Application
import dev.osmo.app.data.CameraRepository

class OsmoApp : Application() {
    val repo: CameraRepository by lazy { CameraRepository(this) }
}
