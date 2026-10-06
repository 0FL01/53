package org.dmsg.client

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
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
                // Stop ownership survives a native call consuming Java interruption.
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

    private fun awaitWaiting(thread: Thread) {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2)
        while (thread.state != Thread.State.TIMED_WAITING && System.nanoTime() < deadline) Thread.yield()
        assertEquals(Thread.State.TIMED_WAITING, thread.state)
    }

    @Test fun wakeBeforeWaitIsStickyAndRepeatedWakesCoalesce() {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val firstWake = CountDownLatch(1)
        val finished = CountDownLatch(1)
        val owner = AtomicReference<Thread>()
        val polls = AtomicInteger()
        lateinit var worker: SingleWorker
        worker = SingleWorker {
            owner.set(Thread.currentThread())
            entered.countDown()
            release.await()
            if (worker.awaitNext(300_000)) polls.incrementAndGet()
            firstWake.countDown()
            if (worker.awaitNext(300_000)) polls.incrementAndGet()
            finished.countDown()
        }
        try {
            worker.start()
            assertTrue(entered.await(2, TimeUnit.SECONDS))
            worker.wake()
            worker.wake()
            release.countDown()
            assertTrue(firstWake.await(2, TimeUnit.SECONDS))
            awaitWaiting(owner.get()) // second wait has no leftover wake
            assertEquals(1, polls.get())
            worker.stop()
            assertTrue(finished.await(2, TimeUnit.SECONDS))
            assertEquals(1, polls.get())
        } finally { release.countDown(); worker.stop() }
    }

    @Test fun wakeDuringEconomyWaitUsesExistingWorkerImmediately() {
        val entered = CountDownLatch(1)
        val woken = CountDownLatch(1)
        val owner = AtomicReference<Thread>()
        val calls = AtomicInteger()
        lateinit var worker: SingleWorker
        worker = SingleWorker {
            calls.incrementAndGet()
            owner.set(Thread.currentThread())
            entered.countDown()
            if (worker.awaitNext(300_000)) woken.countDown()
        }
        try {
            worker.start()
            assertTrue(entered.await(2, TimeUnit.SECONDS))
            awaitWaiting(owner.get())
            worker.wake()
            assertTrue(woken.await(2, TimeUnit.SECONDS))
            assertEquals(1, calls.get())
        } finally { worker.stop() }
    }

    @Test fun stopWinsOverStickyAndLateWakeEvenWhenInterruptWasConsumed() {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val finished = CountDownLatch(1)
        val polls = AtomicInteger()
        lateinit var worker: SingleWorker
        worker = SingleWorker {
            entered.countDown()
            while (release.count > 0) {
                try { release.await() } catch (_: InterruptedException) {}
            }
            Thread.interrupted() // model a native call clearing the stop interrupt
            if (worker.active && worker.awaitNext(300_000)) polls.incrementAndGet()
            finished.countDown()
        }
        try {
            worker.start()
            assertTrue(entered.await(2, TimeUnit.SECONDS))
            worker.wake()
            worker.stop()
            worker.wake()
            release.countDown()
            assertTrue(finished.await(2, TimeUnit.SECONDS))
            assertFalse(worker.running)
            assertEquals(0, polls.get())
        } finally { release.countDown(); worker.stop() }
    }
}
