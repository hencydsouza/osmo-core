package dev.osmo.app.data

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.wifi.WifiNetworkSpecifier
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.suspendCancellableCoroutine

/**
 * Joins the camera's Wi-Fi for this app only and binds the process to it, so the SDK's sockets go to the
 * camera. The camera has no internet; the rest of the phone keeps its normal connection.
 */
class WifiBinder(ctx: Context) {
    private val cm = ctx.getSystemService(ConnectivityManager::class.java)
    private var callback: ConnectivityManager.NetworkCallback? = null

    suspend fun join(ssid: String, password: String?): Network {
        release()
        val spec = WifiNetworkSpecifier.Builder().setSsid(ssid).apply {
            if (!password.isNullOrEmpty()) setWpa2Passphrase(password)
        }.build()
        val request = NetworkRequest.Builder()
            .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
            .removeCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
            .setNetworkSpecifier(spec)
            .build()
        return suspendCancellableCoroutine { cont ->
            val cb = object : ConnectivityManager.NetworkCallback() {
                override fun onAvailable(network: Network) {
                    cm.bindProcessToNetwork(network)
                    if (cont.isActive) cont.resume(network)
                }

                override fun onUnavailable() {
                    if (cont.isActive) cont.resumeWithException(IllegalStateException("Could not join ${ssid}. Is the camera's Wi-Fi on and was the prompt accepted?"))
                }

                override fun onLost(network: Network) {
                    cm.bindProcessToNetwork(null)
                }
            }
            callback = cb
            cm.requestNetwork(request, cb, 45_000)
            cont.invokeOnCancellation { release() }
        }
    }

    fun release() {
        callback?.let { runCatching { cm.unregisterNetworkCallback(it) } }
        callback = null
        cm.bindProcessToNetwork(null)
    }
}
