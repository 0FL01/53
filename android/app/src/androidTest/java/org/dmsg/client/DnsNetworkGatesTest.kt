package org.dmsg.client

import android.content.Context
import android.content.Intent
import android.database.sqlite.SQLiteDatabase
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.net.TrafficStats
import android.os.ParcelFileDescriptor
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TestName
import org.junit.runner.RunWith
import java.io.File
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetAddress
import java.security.MessageDigest
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import uniffi.dmsg_core.DeliveryState

/** Explicit, stateful .gate acceptance. Never run via connected tests or on main.
 * Wi-Fi is restored on-device in finally, even if its ADB control channel drops. */
@RunWith(AndroidJUnit4::class)
class DnsNetworkGatesTest {
    @get:Rule val testName = TestName()
    private fun app(): Context {
        val app = ApplicationProvider.getApplicationContext<Context>()
        assertEquals("org.dmsg.client.gate", app.packageName)
        assertEquals("${javaClass.name}#${testName.methodName}",
            InstrumentationRegistry.getArguments().getString("class"))
        return app
    }
    private fun write(app: Context, name: String, data: JSONObject) {
        val file = File(app.filesDir, name)
        file.writeText(data.toString())
        assertTrue(file.setReadable(false, false)); assertTrue(file.setWritable(false, false))
        assertTrue(file.setReadable(true, true)); assertTrue(file.setWritable(true, true))
    }
    private fun await(label: String, seconds: Long = 45, condition: () -> Boolean) {
        val deadline = System.nanoTime() + seconds * 1_000_000_000
        while (!condition()) {
            assertTrue(label, System.nanoTime() < deadline)
            Thread.sleep(100)
        }
    }
    private fun shell(command: String) {
        ParcelFileDescriptor.AutoCloseInputStream(
            InstrumentationRegistry.getInstrumentation().uiAutomation.executeShellCommand(command)
        ).use { it.readBytes() }
    }
    private fun hash(app: Context, mid: String): String =
        SQLiteDatabase.openDatabase(Core.dbFile(app).absolutePath, null, SQLiteDatabase.OPEN_READONLY).use { db ->
            db.rawQuery("SELECT ciphertext FROM core_outbox WHERE lower(hex(message_id))=?", arrayOf(mid)).use { c ->
                assertTrue(c.moveToFirst())
                MessageDigest.getInstance("SHA-256").digest(c.getBlob(0)).joinToString("") { "%02x".format(it.toInt() and 255) }
            }
        }

    @Test fun actualYandexFallbackPreservesPrimaryAndAccount() {
        val app = app()
        assertFalse(DmsgService.running(app))
        val key = SecureStore.key(app)
        // Context-free facade is test-only: production refresh must not replace the deliberate sink.
        val f = try { UniFfiFacade(Core.dbFile(app).absolutePath, key) } finally { key.fill(0) }
        val before = f.account()
        assertTrue(before.authenticated)
        val primary = f.dnsProfile()!!.resolvers
        val sink = DatagramSocket(0, InetAddress.getByName("127.0.0.1"))
        sink.soTimeout = 100
        val done = AtomicBoolean(false)
        val packets = AtomicLong(0)
        val bytes = AtomicLong(0)
        val counter = Thread {
            val packet = DatagramPacket(ByteArray(65536), 65536)
            while (!done.get()) {
                try {
                    packet.length = packet.data.size
                    sink.receive(packet)
                    packets.incrementAndGet(); bytes.addAndGet(packet.length.toLong())
                } catch (_: java.net.SocketTimeoutException) { }
            }
        }.also { it.start() }
        val uid = android.os.Process.myUid()
        val tx = TrafficStats.getUidTxBytes(uid)
        val rx = TrafficStats.getUidRxBytes(uid)
        val started = System.nanoTime()
        try {
            val failedPrimary = listOf("127.0.0.1:${sink.localPort}")
            f.dnsNetworkChanged(failedPrimary)
            assertTrue(f.reconnect() > 0)
            assertEquals("ready", f.dnsStatus())
            assertEquals(failedPrimary, f.dnsProfile()!!.resolvers)
            val attempted = packets.get()
            assertTrue(attempted > 0)
            f.fetch(); f.retry(); assertTrue(f.reconnect() > 0)
            assertEquals(attempted, packets.get()) // repeated commands must retain backup
            assertEquals(before, f.account())
            assertEquals("ready", f.dnsStatus())
            write(app, "gate-yandex-proof.json", JSONObject()
                .put("primaryPackets", packets.get()).put("primaryDnsPayloadTxBytes", bytes.get())
                .put("uidTxBytes", if (tx >= 0) TrafficStats.getUidTxBytes(uid) - tx else -1)
                .put("uidRxBytes", if (rx >= 0) TrafficStats.getUidRxBytes(uid) - rx else -1)
                .put("elapsedMs", (System.nanoTime() - started) / 1_000_000)
                .put("readyAndKeyResume", true).put("primaryPreserved", true))
        } finally {
            f.dnsNetworkChanged(primary)
            done.set(true); counter.join(); sink.close()
        }
    }

    /** Requires a queued real-peer message and an already enabled cellular data path.
     * Does not enable data/change APN/VPN; restores Wi-Fi and economy preference. */
    @Test fun wifiCellularHandoffAndEconomyWakePreserveQueuedCiphertext() {
        val app = app()
        assertEquals("radio gate requires an independently verified USB ADB channel",
            "usb", InstrumentationRegistry.getArguments().getString("radioControl"))
        val f = Core.facade(app)
        val record = JSONObject(File(app.filesDir, "gate-queued-record").readText())
        val mid = record.getString("mid")
        val before = f.account()
        assertTrue(before.authenticated)
        assertEquals(record.getString("account"), before.contactId)
        assertEquals(record.getString("ciphertextHash"), hash(app, mid))
        assertEquals(DeliveryState.QUEUED, f.messageStatus(mid))
        assertFalse(DmsgService.running(app))
        val cm = app.getSystemService(ConnectivityManager::class.java)
        fun on(transport: Int) = cm.getNetworkCapabilities(cm.activeNetwork)?.hasTransport(transport) == true
        assertTrue("start on Wi-Fi; cellular data must already be enabled", on(NetworkCapabilities.TRANSPORT_WIFI))
        val economy = Prefs.economy(app)
        val proof = JSONObject()
        try {
            assertTrue(f.reconnect() > 0)
            val wifi = DnsNetwork.snapshot(app)!!
            shell("svc wifi disable")
            await("external prerequisite: cellular default network must be available") { on(NetworkCapabilities.TRANSPORT_CELLULAR) }
            val cellular = DnsNetwork.snapshot(app)!!
            assertNotEquals(wifi.network, cellular.network)
            // No FGS: a newly created facade must share state with the old facade.
            assertTrue(Core.facade(app).reconnect() > 0)
            assertEquals("ready", f.dnsStatus())
            assertEquals(record.getString("ciphertextHash"), hash(app, mid))
            assertEquals(DeliveryState.QUEUED, f.messageStatus(mid))
            f.retry()
            assertEquals(DeliveryState.ACCEPTED, f.messageStatus(mid))
            assertEquals(record.getString("ciphertextHash"), hash(app, mid))
            proof.put("foregroundWifiToCellular", true)
                .put("sameDnsAddresses", wifi.resolvers == cellular.resolvers)
            shell("svc wifi enable")
            await("Wi-Fi restoration") { on(NetworkCapabilities.TRANSPORT_WIFI) }
            assertTrue(f.reconnect() > 0)
            proof.put("foregroundCellularToWifi", true)

            Prefs.setEconomy(app, true)
            InstrumentationRegistry.getInstrumentation().startActivitySync(
                Intent(app, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
            DmsgService.start(app)
            await("economy worker first completed poll") {
                DmsgService.running(app) && DmsgService.connectionState().lastSuccessAt != null &&
                    !DmsgService.connectionState().pollInFlight
            }
            // Allow initial callback burst/sticky wake to settle before measuring wake.
            Thread.sleep(2_000)
            for ((transport, command, name) in listOf(
                Triple(NetworkCapabilities.TRANSPORT_CELLULAR, "svc wifi disable", "economyWifiToCellularMs"),
                Triple(NetworkCapabilities.TRANSPORT_WIFI, "svc wifi enable", "economyCellularToWifiMs")
            )) {
                val old = DmsgService.connectionState().revision
                shell(command)
                await("new default network") { on(transport) }
                val start = System.nanoTime()
                await("network wake must bypass 5-minute interval", 60) {
                    val state = DmsgService.connectionState()
                    state.revision > old && state.lastFailure == null && !state.pollInFlight && f.dnsStatus() == "ready"
                }
                proof.put(name, (System.nanoTime() - start) / 1_000_000)
            }
            assertEquals(before, f.account())
            assertEquals(record.getString("ciphertextHash"), hash(app, mid))
            assertEquals(DeliveryState.ACCEPTED, f.messageStatus(mid))
            proof.put("accountPreserved", true).put("midAndCiphertextPreserved", true)
            write(app, "gate-handoff-proof.json", proof)
        } finally {
            shell("svc wifi enable")
            Prefs.setEconomy(app, economy)
            DmsgService.stop(app)
            f.dnsStop()
        }
    }

    /** Non-radio runtime gate: verifies the same worker wake entrypoint while Wi-Fi ADB stays up. */
    @Test fun economyWakeAndStopWithoutRadioChanges() {
        val app = app()
        val f = Core.facade(app)
        val before = f.account()
        assertTrue(before.authenticated)
        assertFalse(DmsgService.running(app))
        val economy = Prefs.economy(app)
        try {
            Prefs.setEconomy(app, true)
            InstrumentationRegistry.getInstrumentation().startActivitySync(
                Intent(app, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
            DmsgService.start(app)
            await("first economy poll") {
                val state = DmsgService.connectionState()
                DmsgService.running(app) && state.lastSuccessAt != null && !state.pollInFlight
            }
            Thread.sleep(2_000)
            val revision = DmsgService.connectionState().revision
            val start = System.nanoTime()
            DmsgService.wakeAfterNetworkApplied()
            await("wake bypasses 5-minute sleep", 30) {
                val state = DmsgService.connectionState()
                state.revision > revision && !state.pollInFlight && state.lastFailure == null
            }
            val elapsed = (System.nanoTime() - start) / 1_000_000
            DmsgService.stop(app)
            await("worker stops") { !DmsgService.running(app) }
            Thread.sleep(500)
            val stopped = DmsgService.connectionState().revision
            DmsgService.wakeAfterNetworkApplied()
            Thread.sleep(2_000)
            assertEquals(stopped, DmsgService.connectionState().revision)
            assertEquals("stopped", f.dnsStatus())
            assertEquals(before, f.account())
            write(app, "gate-wake-proof.json", JSONObject().put("wakeMs", elapsed)
                .put("noLatePollAfterStop", true).put("accountPreserved", true))
        } finally { Prefs.setEconomy(app, economy); DmsgService.stop(app); f.dnsStop() }
    }

    @Test fun retryQueuedPreservesCiphertextWithoutRadioChanges() {
        val app = app()
        assertFalse(DmsgService.running(app))
        val key = SecureStore.key(app)
        val f = try { UniFfiFacade(Core.dbFile(app).absolutePath, key) } finally { key.fill(0) }
        val primary = f.dnsProfile()!!.resolvers
        val record = JSONObject(File(app.filesDir, "gate-queued-record").readText())
        val mid = record.getString("mid")
        DatagramSocket(0, InetAddress.getByName("127.0.0.1")).use { sink ->
            try {
                // Local unread sink forces the PRODUCTION Yandex fallback, not an ADB bridge.
                f.dnsNetworkChanged(listOf("127.0.0.1:${sink.localPort}"))
                assertEquals(record.getString("account"), f.account().contactId)
                assertEquals(record.getString("ciphertextHash"), hash(app, mid))
                assertTrue(f.messageStatus(mid) in listOf(DeliveryState.QUEUED, DeliveryState.ACCEPTED))
                f.retry()
                assertEquals("ready", f.dnsStatus())
                assertEquals(DeliveryState.ACCEPTED, f.messageStatus(mid))
                assertEquals(record.getString("ciphertextHash"), hash(app, mid))
                assertTrue(f.retry()[1] >= 1)
                assertEquals(record.getString("ciphertextHash"), hash(app, mid))
                write(app, "gate-retry-proof.json", JSONObject().put("midAndCiphertextPreserved", true)
                    .put("accepted", true).put("repeatSameCiphertext", true).put("actualYandexFallback", true))
            } finally { f.dnsNetworkChanged(primary) }
        }
    }
}
