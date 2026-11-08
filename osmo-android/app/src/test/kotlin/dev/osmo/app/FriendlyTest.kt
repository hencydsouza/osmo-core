package dev.osmo.app

import dev.osmo.app.data.friendly
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.osmo_ffi.OsmoException

class FriendlyTest {
    @Test fun silenceFromCameraBecomesAnAdvice() {
        val m = OsmoException.Session("no response from camera").friendly()
        assertTrue(m.contains("isn’t answering"))
    }

    @Test fun otherSessionErrorsSuggestReconnect() {
        assertTrue(OsmoException.Session("socket closed").friendly().contains("Reconnect"))
    }

    @Test fun internalDetailNeverLeaksToTheUser() {
        val m = OsmoException.Internal("panic at src/foo.rs:12").friendly()
        assertFalse(m.contains("foo.rs"))
    }

    @Test fun plainExceptionsKeepTheirMessage() {
        assertTrue(IllegalStateException("boom").friendly() == "boom")
    }
}
