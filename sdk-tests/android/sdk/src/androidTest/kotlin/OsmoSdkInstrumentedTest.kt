package dev.osmo.sdk

import android.media.MediaMetadataRetriever
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import org.junit.Assert.*
import org.junit.Test
import uniffi.osmo_ffi.*

class OsmoSdkInstrumentedTest {
    private val ctx = InstrumentationRegistry.getInstrumentation().context
    private val cache = InstrumentationRegistry.getInstrumentation().targetContext.cacheDir

    private fun fixture(name: String): String {
        val out = File(cache, name)
        ctx.assets.open(name).use { i -> out.outputStream().use { i.copyTo(it) } }
        return out.path
    }

    private fun tmp(name: String) = File(cache, "${System.nanoTime()}-$name").path

    @Test fun libraryLoadsAndReportsVersion() {
        assertTrue(sdkVersion().isNotEmpty())
    }

    @Test fun trimFixtureOnDevice() {
        for (name in listOf("h264_aac.mp4", "hevc_aac.mp4", "faststart.mp4")) {
            val out = tmp("clip.mp4")
            val r = trimLocalFile(fixture(name), 0.5, 1.5, out, null)
            assertTrue(r.bytes > 0uL)
            val mmr = MediaMetadataRetriever()
            mmr.setDataSource(out)
            val ms = mmr.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)!!.toLong()
            assertEquals(name, r.durationSecs, ms / 1000.0, 0.3)
            mmr.release()
        }
    }

    @Test fun frameClipExtractsAFrame() {
        val out = tmp("frame.mp4")
        trimLocalFile(fixture("h264_aac.mp4"), 1.0, 1.5, out, null)
        val mmr = MediaMetadataRetriever()
        mmr.setDataSource(out)
        assertNotNull(mmr.getFrameAtTime(0))
        mmr.release()
    }

    @Test fun badRangeIsATypedError() {
        val src = fixture("h264_aac.mp4")
        try { trimLocalFile(src, -1.0, 1.0, tmp("x.mp4"), null); fail() } catch (e: OsmoException.Refused) {}
        try { trimLocalFile(src, Double.NaN, 1.0, tmp("x.mp4"), null); fail() } catch (e: OsmoException.Refused) {}
        try { trimLocalFile(src, 5.0, 1.0, tmp("x.mp4"), null); fail() } catch (e: OsmoException.Mp4) {}
    }

    @Test fun mockBleLinkRoundTrip() {
        val written = mutableListOf<ByteArray>()
        val link = object : BleLink {
            override fun writeCommand(bytes: ByteArray) { written.add(bytes); throw OsmoException.Ble("device disconnected") }
            override fun recv(timeoutMs: UInt): ByteArray? = null
        }
        try { readWifiCredentials(link); fail() } catch (e: OsmoException.Ble) {}
        assertTrue(written.isNotEmpty())
    }

    @Test fun progressCancel() {
        var calls = 0
        val stop = object : ProgressListener {
            override fun onProgress(done: ULong, total: ULong): Boolean { calls++; return false }
        }
        try { trimLocalFile(fixture("h264_aac.mp4"), 0.0, 1.5, tmp("c.mp4"), stop); fail() } catch (e: OsmoException.Cancelled) {}
        assertTrue(calls > 0)
    }
}
