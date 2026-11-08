package dev.osmo.app.ui.home

import androidx.lifecycle.ViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import uniffi.osmo_ffi.sdkVersion

data class HomeState(val sdkVersion: String)

class HomeViewModel(version: () -> String = { sdkVersion() }) : ViewModel() {
    private val _state = MutableStateFlow(HomeState(sdkVersion = runCatching(version).getOrDefault("unavailable")))
    val state: StateFlow<HomeState> = _state
}
