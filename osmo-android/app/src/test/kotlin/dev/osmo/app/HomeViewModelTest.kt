package dev.osmo.app

import dev.osmo.app.ui.home.HomeViewModel
import org.junit.Assert.assertEquals
import org.junit.Test

class HomeViewModelTest {
    @Test fun exposesSdkVersion() {
        assertEquals("1.2.3", HomeViewModel { "1.2.3" }.state.value.sdkVersion)
    }

    @Test fun missingNativeLibraryDoesNotCrash() {
        assertEquals("unavailable", HomeViewModel { throw UnsatisfiedLinkError("no lib") }.state.value.sdkVersion)
    }
}
