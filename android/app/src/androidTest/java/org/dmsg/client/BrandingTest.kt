package org.dmsg.client

import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.drawable.AdaptiveIconDrawable
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Test
import java.io.File

/** Package/renderer inspection only: no core, database writes or identity reset. */
class BrandingTest {
    @Test fun installed53PackageHasSuppliedIcon() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        assertTrue(context.packageName == "org.dmsg.client" || context.packageName == "org.dmsg.client.gate")
        val manager = context.packageManager
        val info = manager.getApplicationInfo(context.packageName, 0)
        assertEquals("53", manager.getApplicationLabel(info).toString())
        assertTrue(info.icon != 0)
        val icon = manager.getApplicationIcon(info)
        assertTrue("API26+ launcher must have a mask-safe adaptive icon", icon is AdaptiveIconDrawable)
        val bitmap = Bitmap.createBitmap(256, 256, Bitmap.Config.ARGB_8888)
        icon.setBounds(0, 0, 256, 256)
        icon.draw(Canvas(bitmap))
        var blue = 0
        var parchment = 0
        for (y in 0 until 256) for (x in 0 until 256) {
            val pixel = bitmap.getPixel(x, y)
            val red = pixel shr 16 and 255
            val green = pixel shr 8 and 255
            val b = pixel and 255
            if (b > red + 30 && green > red + 15) blue++
            if (red > 170 && red > b + 4 && green > b) parchment++
        }
        assertTrue("supplied blue feather missing", blue > 1000)
        assertTrue("supplied parchment missing", parchment > 1000)
        File(context.cacheDir, "53-launcher-preview.png").outputStream().use {
            assertTrue(bitmap.compress(Bitmap.CompressFormat.PNG, 100, it))
        }
    }
}
