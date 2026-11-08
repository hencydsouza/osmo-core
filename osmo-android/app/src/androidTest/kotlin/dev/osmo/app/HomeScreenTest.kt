package dev.osmo.app

import androidx.compose.ui.test.junit4.v2.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithText
import org.junit.Rule
import org.junit.Test

class HomeScreenTest {
    @get:Rule val rule = createAndroidComposeRule<MainActivity>()

    @Test fun showsHomeAndRealNativeSdkVersion() {
        rule.onNodeWithText("No camera connected").assertExists()
        // The real libosmo_ffi.so loads and answers on the device.
        rule.onNodeWithText("SDK", substring = true).assertExists()
    }
}
