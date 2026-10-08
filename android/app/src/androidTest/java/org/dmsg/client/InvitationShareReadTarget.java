package org.dmsg.client;

import android.app.Activity;
import android.content.Intent;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.net.Uri;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;

/** Standalone test-APK UID: platform/Java only, no target-APK or Kotlin runtime dependency. */
public final class InvitationShareReadTarget extends Activity {
    @Override public void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        new Handler(Looper.getMainLooper()).postDelayed(this::readGrant, 1500);
    }

    private void readGrant() {
        boolean ok = false;
        byte[] bytes = new byte[8 * 1024 * 1024 + 1];
        Bitmap bitmap = null;
        int[] pixels = null;
        try {
            Intent input = getIntent();
            Uri uri = input.getClipData() != null && input.getClipData().getItemCount() == 1
                ? input.getClipData().getItemAt(0).getUri() : null;
            require("image/png".equals(input.getType()) && uri != null);
            require(input.getStringExtra(Intent.EXTRA_TEXT) == null);
            require((input.getFlags() & Intent.FLAG_GRANT_WRITE_URI_PERMISSION) == 0);
            int count = 0;
            try (InputStream stream = getContentResolver().openInputStream(uri)) {
                require(stream != null);
                while (count < bytes.length) {
                    int next = stream.read(bytes, count, bytes.length - count);
                    if (next < 0) break;
                    if (next == 0) {
                        int single = stream.read();
                        if (single < 0) break;
                        bytes[count++] = (byte) single;
                    } else count += next;
                }
            }
            require(count >= 8 && count < bytes.length);
            byte[] signature = {(byte) 137, 80, 78, 71, 13, 10, 26, 10};
            for (int i = 0; i < signature.length; i++) require(bytes[i] == signature[i]);
            BitmapFactory.Options bounds = new BitmapFactory.Options();
            bounds.inJustDecodeBounds = true;
            BitmapFactory.decodeByteArray(bytes, 0, count, bounds);
            require(bounds.outWidth == 512 && bounds.outHeight == 512 && "image/png".equals(bounds.outMimeType));
            BitmapFactory.Options options = new BitmapFactory.Options();
            options.inMutable = true;
            bitmap = BitmapFactory.decodeByteArray(bytes, 0, count, options);
            require(bitmap != null);
            pixels = new int[512 * 512];
            bitmap.getPixels(pixels, 0, 512, 0, 0, 512, 512);
            boolean black = false;
            boolean white = false;
            for (int pixel : pixels) {
                black |= pixel == android.graphics.Color.BLACK;
                white |= pixel == android.graphics.Color.WHITE;
            }
            require(black && white);
            ok = true;
        } catch (Exception ignored) {
            // Static evidence only; no provider URI/image/content in diagnostics.
        } finally {
            Arrays.fill(bytes, (byte) 0);
            if (pixels != null) Arrays.fill(pixels, 0);
            if (bitmap != null) { bitmap.eraseColor(0); bitmap.recycle(); }
        }
        try {
            File proof = new File(getFilesDir(), "invitation-share-read-proof.json");
            try (FileOutputStream output = new FileOutputStream(proof)) {
                output.write((ok ? "{\"readPngAfterPause\":true}" : "{\"readPngAfterPause\":false}")
                    .getBytes(StandardCharsets.US_ASCII));
            }
        } catch (Exception ignored) { }
        sendBroadcast(new Intent("org.dmsg.client.gate.INVITATION_SHARE_READ")
            .setPackage("org.dmsg.client.gate").putExtra("readPngAfterPause", ok));
        finish();
    }

    private static void require(boolean condition) {
        if (!condition) throw new IllegalArgumentException("Invalid PNG grant");
    }
}
