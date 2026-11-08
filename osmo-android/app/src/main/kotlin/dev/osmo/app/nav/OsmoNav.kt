package dev.osmo.app.nav

import androidx.compose.runtime.Composable
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.remember
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.ui.NavDisplay
import dev.osmo.app.ui.home.HomeScreen
import kotlinx.serialization.Serializable

@Serializable data object Home : NavKey

/** Single back stack for now; Library/Settings destinations are added as the screens land. */
@Composable
fun OsmoNav() {
    val backStack = remember { mutableStateListOf<NavKey>(Home) }
    NavDisplay(
        backStack = backStack,
        onBack = { if (backStack.size > 1) backStack.removeLastOrNull() },
        entryProvider = entryProvider {
            entry<Home> { HomeScreen() }
        },
    )
}
