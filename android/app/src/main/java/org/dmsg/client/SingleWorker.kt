package org.dmsg.client

/** A stopped blocking call may finish, but its replacement never overlaps it. */
internal class SingleWorker(private val work: () -> Unit) {
    @Volatile private var wanted = false
    private var thread: Thread? = null

    val running: Boolean get() = wanted

    @Synchronized fun start() {
        wanted = true
        if (thread == null) launch()
    }

    @Synchronized fun stop() {
        wanted = false
        // Keep ownership until finally: native blocking calls cannot be cancelled.
        thread?.interrupt()
    }

    private fun launch() {
        thread = Thread({
            try {
                work()
            } finally {
                synchronized(this) {
                    thread = null
                    if (wanted && Thread.currentThread().isInterrupted) launch()
                    else wanted = false
                }
            }
        }, "dmsg-poll").also { it.isDaemon = true; it.start() }
    }
}
