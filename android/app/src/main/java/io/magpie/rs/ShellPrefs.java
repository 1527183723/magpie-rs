package io.magpie.rs;

import android.content.Context;

import org.json.JSONObject;

import java.io.File;
import java.io.FileInputStream;
import java.io.ByteArrayOutputStream;
import java.nio.charset.StandardCharsets;

/**
 * The two shell settings Java needs before the native library is loaded:
 * whether to start on boot, and what to print in the notification.
 *
 * <p>The file is written by Rust ({@code files/shell.json}); this reader is
 * deliberately forgiving so a hand-edited or half-written file falls back to the
 * defaults instead of crashing a broadcast receiver.
 */
final class ShellPrefs {

    private static JSONObject read(Context ctx) {
        File f = new File(ctx.getFilesDir(), "shell.json");
        if (!f.isFile()) {
            return new JSONObject();
        }
        try (FileInputStream in = new FileInputStream(f)) {
            ByteArrayOutputStream buf = new ByteArrayOutputStream();
            byte[] chunk = new byte[4096];
            int n;
            while ((n = in.read(chunk)) > 0) {
                buf.write(chunk, 0, n);
            }
            return new JSONObject(new String(buf.toByteArray(), StandardCharsets.UTF_8));
        } catch (Exception e) {
            return new JSONObject();
        }
    }

    static boolean autostart(Context ctx) {
        return read(ctx).optBoolean("autostart", false);
    }

    static boolean lan(Context ctx) {
        return read(ctx).optBoolean("lan", false);
    }

    static int gatewayPort(Context ctx) {
        int p = read(ctx).optInt("gateway_port", 3425);
        return p >= 1024 && p <= 65535 ? p : 3425;
    }

    static int webPort(Context ctx) {
        int p = read(ctx).optInt("web_port", 3430);
        return p >= 1024 && p <= 65535 ? p : 3430;
    }

    private ShellPrefs() {
    }
}
