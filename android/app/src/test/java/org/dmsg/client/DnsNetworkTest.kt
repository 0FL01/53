package org.dmsg.client

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.*
import org.junit.Test

class DnsNetworkTest {
    private val dns = listOf("192.0.2.1:53")

    @Test fun facadeInstancesSharePerDatabaseRuntime() {
        assertSame(DnsNetwork.runtime("test-db-a"), DnsNetwork.runtime("test-db-a"))
        assertNotSame(DnsNetwork.runtime("test-db-a"), DnsNetwork.runtime("test-db-b"))
    }

    @Test fun sameDnsNewNetworkChangesButDuplicateAndLateEventsDoNot() {
        val state = DnsRuntimeState<String>()
        var active = DnsSnapshot("wifi", dns)
        var cancellations = 0
        var wakes = 0
        val applications = mutableListOf<List<String>>()
        // Like both foreground refresh and the callback, sample the current network,
        // never the Network carried by an old onLost/onLinkPropertiesChanged event.
        fun event(@Suppress("UNUSED_PARAMETER") eventNetwork: String) {
            state.observe({ active }) { cancellations++ }
            state.apply({ applications.add(it) }) { wakes++ }
        }
        event("wifi")
        event("wifi")
        assertEquals(1, cancellations)
        active = DnsSnapshot("lte", dns)
        event("lte")
        event("wifi") // late loss/properties from the old network
        event("lte")
        assertEquals(2, cancellations)
        assertEquals(listOf(dns, dns), applications)
        assertEquals(2, wakes)
        active = DnsSnapshot("lte", listOf("192.0.2.2:53"))
        event("lte")
        assertEquals(3, cancellations)
        assertEquals(3, applications.size)
    }

    @Test fun networkLossAndMissingDnsCancelOnceAndPreservePrimaryUntilRecovery() {
        val state = DnsRuntimeState<String>()
        var cancellations = 0
        var wakes = 0
        val applications = mutableListOf<List<String>>()
        fun observe(snapshot: DnsSnapshot<String>?) {
            state.observe({ snapshot }) { cancellations++ }
            state.apply({ applications.add(it) }) { wakes++ }
        }
        observe(DnsSnapshot("wifi", dns))
        observe(null)
        observe(null)
        observe(DnsSnapshot("lte", emptyList()))
        observe(DnsSnapshot("lte", emptyList()))
        assertEquals(3, cancellations)
        assertEquals(listOf(dns), applications)
        assertEquals(1, wakes)
        observe(DnsSnapshot("lte", dns))
        assertEquals(4, cancellations)
        assertEquals(listOf(dns, dns), applications)
        assertEquals(2, wakes)
    }

    @Test fun observationCancelsBeforeStoreLockAndApplicationUsesLatestSnapshot() {
        val state = DnsRuntimeState<String>()
        val cancellations = AtomicInteger()
        val held = CountDownLatch(1)
        val release = CountDownLatch(1)
        val applied = CountDownLatch(1)
        val applications = mutableListOf<List<String>>()
        val store = Thread {
            synchronized(Core.storeLock) {
                held.countDown()
                assertTrue(release.await(2, TimeUnit.SECONDS))
            }
        }
        store.start()
        assertTrue(held.await(2, TimeUnit.SECONDS))
        try {
            state.observe({ DnsSnapshot("wifi", dns) }) { cancellations.incrementAndGet() }
            val transition = Thread {
                synchronized(Core.storeLock) {
                    state.apply({ applications.add(it) }) { applied.countDown() }
                }
            }
            transition.start()
            val latest = listOf("192.0.2.2:53")
            state.observe({ DnsSnapshot("lte", latest) }) { cancellations.incrementAndGet() }
            assertEquals(2, cancellations.get())
            assertEquals(1L, applied.count)
            release.countDown()
            assertTrue(applied.await(2, TimeUnit.SECONDS))
            transition.join(2_000)
            assertFalse(transition.isAlive)
            assertEquals(listOf(latest), applications)
        } finally { release.countDown(); store.join(2_000) }
    }

    @Test fun newerObservationDuringApplyCancelsImmediatelyAndOnlyLatestApplyWakes() {
        val state = DnsRuntimeState<String>()
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val done = CountDownLatch(1)
        val cancellations = AtomicInteger()
        val wakes = AtomicInteger()
        val applications = mutableListOf<List<String>>()
        val latest = listOf("192.0.2.2:53")
        state.observe({ DnsSnapshot("wifi", dns) }) { cancellations.incrementAndGet() }
        val apply = Thread {
            synchronized(Core.storeLock) {
                state.apply({
                    applications.add(it)
                    if (applications.size == 1) {
                        entered.countDown()
                        assertTrue(release.await(2, TimeUnit.SECONDS))
                    }
                }) { wakes.incrementAndGet() }
            }
            done.countDown()
        }
        apply.start()
        try {
            assertTrue(entered.await(2, TimeUnit.SECONDS))
            state.observe({ DnsSnapshot("lte", latest) }) { cancellations.incrementAndGet() }
            assertEquals(2, cancellations.get())
            release.countDown()
            assertTrue(done.await(2, TimeUnit.SECONDS))
            assertEquals(listOf(dns, latest), applications)
            assertEquals(1, wakes.get())
            state.apply({ fail("duplicate application") }) { fail("duplicate wake") }
        } finally { release.countDown(); apply.join(2_000) }
    }
}
