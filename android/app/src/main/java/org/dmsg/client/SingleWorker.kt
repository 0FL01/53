package org.dmsg.client

/** A stopped blocking call may finish, but its replacement never overlaps it. */
internal class SingleWorker(private val work: () -> Unit) {
    private val lock = java.lang.Object()
    @Volatile private var wanted = false
    private var thread: Thread? = null
    private var stopping = false
    private var pendingWake = false

    val running: Boolean get() = wanted
    val active: Boolean get() = synchronized(lock) {
        wanted && !stopping && !Thread.currentThread().isInterrupted
    }

    fun start() = synchronized(lock) {
        wanted = true
        if (thread == null) launch()
    }

    fun stop() = synchronized(lock) {
        wanted = false
        stopping = true
        pendingWake = false
        // Keep ownership until finally, even if native work consumes the interrupt.
        thread?.interrupt()
        lock.notifyAll()
    }

    fun wake() = synchronized(lock) {
        if (wanted && !stopping) {
            pendingWake = true
            lock.notifyAll()
        }
    }

    /** A wake before wait is sticky; repeated wakes coalesce. Stop always wins. */
    fun awaitNext(delayMs: Long): Boolean = synchronized(lock) {
        val deadline = System.nanoTime() + java.util.concurrent.TimeUnit.MILLISECONDS.toNanos(delayMs)
        try {
            while (active && !pendingWake) {
                val remaining = deadline - System.nanoTime()
                if (remaining <= 0) break
                lock.wait(remaining / 1_000_000, (remaining % 1_000_000).toInt())
            }
        } catch (_: InterruptedException) {
            Thread.currentThread().interrupt()
        }
        pendingWake = false
        active
    }

    private fun launch() {
        stopping = false
        pendingWake = false
        thread = Thread({
            try {
                work()
            } finally {
                synchronized(lock) {
                    thread = null
                    if (wanted && stopping) launch()
                    else wanted = false
                }
            }
        }, "dmsg-poll").also { it.isDaemon = true; it.start() }
    }
}
