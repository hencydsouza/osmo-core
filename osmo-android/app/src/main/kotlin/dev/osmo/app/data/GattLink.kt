package dev.osmo.app.data

import android.annotation.SuppressLint
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothGatt
import android.bluetooth.BluetoothGattCallback
import android.bluetooth.BluetoothGattCharacteristic
import android.bluetooth.BluetoothGattDescriptor
import android.bluetooth.BluetoothProfile
import android.content.Context
import android.os.Build
import java.util.UUID
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import uniffi.osmo_ffi.BleLink
import uniffi.osmo_ffi.OsmoException

private fun u16(short: Int) = UUID.fromString(String.format("0000%04x-0000-1000-8000-00805f9b34fb", short))
private val SERVICE = u16(0xFFF0)
private val FFF4 = u16(0xFFF4)
private val FFF5 = u16(0xFFF5)
private val CCCD = u16(0x2902)

/**
 * Blocking GATT channel the Rust SDK drives (same contract as osmo-core `BleLink`): connect, enable
 * notifications on fff4/fff5, arm pairing, then move DUML bytes. All methods block; call from a worker thread.
 */
@SuppressLint("MissingPermission")
class GattLink(private val ctx: Context, private val device: BluetoothDevice) : BleLink, AutoCloseable {
    private sealed interface Ev {
        data class Connected(val ok: Boolean) : Ev
        data class Services(val ok: Boolean) : Ev
        data class Desc(val ok: Boolean) : Ev
        data class Wrote(val ok: Boolean) : Ev
    }

    private val events = LinkedBlockingQueue<Ev>()
    private val rx = LinkedBlockingQueue<ByteArray>()
    @Volatile private var gatt: BluetoothGatt? = null
    @Volatile private var closed = false
    @Volatile private var dropped = false
    private var fff4: BluetoothGattCharacteristic? = null
    private var fff5: BluetoothGattCharacteristic? = null

    private val cb = object : BluetoothGattCallback() {
        override fun onConnectionStateChange(g: BluetoothGatt, status: Int, newState: Int) {
            if (newState == BluetoothProfile.STATE_CONNECTED) events.add(Ev.Connected(status == BluetoothGatt.GATT_SUCCESS))
            else { dropped = true; events.add(Ev.Connected(false)) }
        }
        override fun onServicesDiscovered(g: BluetoothGatt, status: Int) { events.add(Ev.Services(status == BluetoothGatt.GATT_SUCCESS)) }
        override fun onDescriptorWrite(g: BluetoothGatt, d: BluetoothGattDescriptor, status: Int) { events.add(Ev.Desc(status == BluetoothGatt.GATT_SUCCESS)) }
        override fun onCharacteristicWrite(g: BluetoothGatt, c: BluetoothGattCharacteristic, status: Int) { events.add(Ev.Wrote(status == BluetoothGatt.GATT_SUCCESS)) }
        override fun onCharacteristicChanged(g: BluetoothGatt, c: BluetoothGattCharacteristic, value: ByteArray) { rx.add(value) }
        @Deprecated("API < 33")
        override fun onCharacteristicChanged(g: BluetoothGatt, c: BluetoothGattCharacteristic) {
            if (Build.VERSION.SDK_INT < 33) @Suppress("DEPRECATION") rx.add(c.value ?: return)
        }
    }

    private inline fun <reified T : Ev> await(what: String, seconds: Long = 10): T {
        val end = System.nanoTime() + TimeUnit.SECONDS.toNanos(seconds)
        while (true) {
            val left = end - System.nanoTime()
            val ev = events.poll(left.coerceAtLeast(0), TimeUnit.NANOSECONDS)
                ?: throw OsmoException.Ble("timed out waiting for $what")
            if (ev is T) return ev
            if (ev is Ev.Connected && !ev.ok) throw OsmoException.Ble("camera disconnected during $what")
        }
    }

    /** Connect, subscribe and arm pairing. Throws [OsmoException.Ble] on failure. */
    fun open() {
        val g = device.connectGatt(ctx, false, cb, BluetoothDevice.TRANSPORT_LE)
            ?: throw OsmoException.Ble("could not start a Bluetooth connection")
        gatt = g
        if (!await<Ev.Connected>("connection", 15).ok) throw OsmoException.Ble("could not connect to the camera")
        g.requestMtu(247)
        if (!g.discoverServices()) throw OsmoException.Ble("could not discover services")
        if (!await<Ev.Services>("service discovery", 15).ok) throw OsmoException.Ble("service discovery failed")
        val svc = g.getService(SERVICE) ?: throw OsmoException.Ble("camera does not expose the control service")
        val c4 = svc.getCharacteristic(FFF4)
        val c5 = svc.getCharacteristic(FFF5)
        if (c4 == null || c5 == null) throw OsmoException.Ble("camera does not expose the fff4/fff5 characteristics")
        fff4 = c4; fff5 = c5
        subscribe(g, c4)
        subscribe(g, c5)
        // DUML pairing arm, same bytes as the CLI (osmo-cli/src/ble_btleplug.rs).
        write(g, c4, byteArrayOf(0x01, 0x00), BluetoothGattCharacteristic.WRITE_TYPE_DEFAULT)
        if (!await<Ev.Wrote>("pairing arm").ok) throw OsmoException.Ble("could not arm pairing")
    }

    private fun subscribe(g: BluetoothGatt, c: BluetoothGattCharacteristic) {
        g.setCharacteristicNotification(c, true)
        val d = c.getDescriptor(CCCD) ?: throw OsmoException.Ble("missing notification descriptor")
        val value = if (c.properties and BluetoothGattCharacteristic.PROPERTY_NOTIFY != 0)
            BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE else BluetoothGattDescriptor.ENABLE_INDICATION_VALUE
        val started = if (Build.VERSION.SDK_INT >= 33) g.writeDescriptor(d, value) == BluetoothGatt.GATT_SUCCESS
        else @Suppress("DEPRECATION") run { d.value = value; g.writeDescriptor(d) }
        if (!started) throw OsmoException.Ble("could not enable notifications")
        if (!await<Ev.Desc>("notification setup").ok) throw OsmoException.Ble("notification setup failed")
    }

    private fun write(g: BluetoothGatt, c: BluetoothGattCharacteristic, bytes: ByteArray, type: Int): Boolean =
        if (Build.VERSION.SDK_INT >= 33) g.writeCharacteristic(c, bytes, type) == BluetoothGatt.GATT_SUCCESS
        else @Suppress("DEPRECATION") run { c.writeType = type; c.value = bytes; g.writeCharacteristic(c) }

    override fun writeCommand(bytes: ByteArray) {
        val g = gatt; val c = fff5
        if (closed || dropped || g == null || c == null) throw OsmoException.Ble("camera disconnected")
        // Write-without-response can be refused while the stack is busy; retry briefly.
        repeat(50) {
            if (write(g, c, bytes, BluetoothGattCharacteristic.WRITE_TYPE_NO_RESPONSE)) return
            Thread.sleep(10)
        }
        throw OsmoException.Ble("could not write to the camera")
    }

    override fun recv(timeoutMs: UInt): ByteArray? {
        rx.poll(timeoutMs.toLong(), TimeUnit.MILLISECONDS)?.let { return it }
        if (dropped || closed) throw OsmoException.Ble("camera disconnected")
        return null
    }

    override fun close() {
        closed = true
        gatt?.let { runCatching { it.disconnect() }; runCatching { it.close() } }
        gatt = null
    }
}
