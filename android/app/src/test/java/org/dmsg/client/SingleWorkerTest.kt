package org.dmsg.client

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.*
import org.junit.Test

class SingleWorkerTest {
    @Test fun stopStartWaitsForUninterruptibleWork() {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val restarted = CountDownLatch(1)
        val calls = AtomicInteger()
        val active = AtomicInteger()
        val maximum = AtomicInteger()
        val worker = SingleWorker {
            maximum.updateAndGet { maxOf(it, active.incrementAndGet()) }
            if (calls.incrementAndGet() == 1) {
                entered.countDown()
                // Model a native call that ignores Java interruption.
                while (release.count > 0) {
                    try { release.await() } catch (_: InterruptedException) {}
                }
                Thread.currentThread().interrupt()
            } else restarted.countDown()
            active.decrementAndGet()
        }
        try {
            worker.start()
            assertTrue(entered.await(2, TimeUnit.SECONDS))
            worker.stop()
            worker.start()
            worker.start()
            assertEquals(1, calls.get())
            release.countDown()
            assertTrue(restarted.await(2, TimeUnit.SECONDS))
            assertEquals(1, maximum.get())
        } finally { release.countDown(); worker.stop() }
    }
}
