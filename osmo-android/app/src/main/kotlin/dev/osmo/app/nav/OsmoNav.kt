package dev.osmo.app.nav

import androidx.compose.runtime.Composable
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.remember
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import androidx.lifecycle.viewmodel.CreationExtras
import androidx.lifecycle.ViewModelProvider.AndroidViewModelFactory.Companion.APPLICATION_KEY
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.ui.NavDisplay
import dev.osmo.app.OsmoApp
import dev.osmo.app.ui.connect.ConnectScreen
import dev.osmo.app.ui.connect.ConnectViewModel
import dev.osmo.app.ui.detail.DetailScreen
import dev.osmo.app.ui.detail.DetailViewModel
import dev.osmo.app.ui.library.LibraryScreen
import dev.osmo.app.ui.library.LibraryViewModel
import dev.osmo.app.ui.local.LocalScreen
import dev.osmo.app.ui.local.LocalViewModel
import kotlinx.serialization.Serializable
import uniffi.osmo_ffi.MediaFile
import uniffi.osmo_ffi.MediaKind
import uniffi.osmo_ffi.sdkVersion

@Serializable data object Connect : NavKey
@Serializable data object Library : NavKey
@Serializable data object Local : NavKey
@Serializable data object Gps : NavKey
@Serializable data class Detail(val path: String, val sizeBytes: Long, val starred: Boolean, val video: Boolean, val storage: Int) : NavKey

private fun Detail.file() = MediaFile(path, sizeBytes.toULong(), starred, if (video) MediaKind.VIDEO else MediaKind.PHOTO, storage.toUByte())

@Composable
fun OsmoNav() {
    val backStack = remember { mutableStateListOf<NavKey>(Connect) }
    val repoFactory = viewModelFactory {
        initializer { ConnectViewModel((this[APPLICATION_KEY] as OsmoApp).repo) }
        initializer { LibraryViewModel((this[APPLICATION_KEY] as OsmoApp).repo) }
        initializer { LocalViewModel((this[APPLICATION_KEY] as OsmoApp).repo) }
    }
    NavDisplay(
        backStack = backStack,
        onBack = { if (backStack.size > 1) backStack.removeLastOrNull() },
        entryProvider = entryProvider {
            entry<Connect> {
                val vm: ConnectViewModel = viewModel(factory = repoFactory)
                ConnectScreen(vm, sdkVersion(), onLocal = { backStack.add(Local) }, onGps = { backStack.add(Gps) }) { backStack.add(Library) }
            }
            entry<Library> {
                val vm: LibraryViewModel = viewModel(factory = repoFactory)
                LibraryScreen(
                    vm,
                    onOpen = { f -> backStack.add(Detail(f.path, f.sizeBytes.toLong(), f.starred, f.kind == MediaKind.VIDEO, f.storage.toInt())) },
                    onLocal = { backStack.add(Local) },
                    onBack = { backStack.removeLastOrNull() },
                )
            }
            entry<Gps> {
                val app = androidx.compose.ui.platform.LocalContext.current.applicationContext as OsmoApp
                dev.osmo.app.ui.gps.GpsScreen(app.gps) { backStack.removeLastOrNull() }
            }
            entry<Local> {
                val vm: LocalViewModel = viewModel(factory = repoFactory)
                LocalScreen(vm) { backStack.removeLastOrNull() }
            }
            entry<Detail> { key ->
                val app = androidx.compose.ui.platform.LocalContext.current.applicationContext as OsmoApp
                dev.osmo.app.ui.detail.DetailPager(app.repo, key.file()) { backStack.removeLastOrNull() }
            }
        },
    )
}
