package io.magpie.rs;

import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageInstaller;

import java.io.File;
import java.io.FileInputStream;
import java.io.InputStream;
import java.io.OutputStream;

/**
 * Installs a downloaded APK through a {@link PackageInstaller} session.
 *
 * <p>This is the platform's own self-update path: no {@code FileProvider}, no
 * {@code REQUEST_INSTALL_PACKAGES} dance for a file URI, and the system installer
 * shows the usual confirmation. Rust decides *that* there is an update and
 * downloads it; this class only hands the bytes over.
 */
public final class ApkInstaller {

    private ApkInstaller() {
    }

    /** Called from Rust with the absolute path of a downloaded APK. */
    public static void install(Context ctx, String path) {
        File apk = new File(path);
        if (!apk.isFile() || apk.length() == 0) {
            ShellUi.toast(ctx, ctx.getString(R.string.update_missing));
            return;
        }
        try {
            PackageInstaller installer = ctx.getPackageManager().getPackageInstaller();
            PackageInstaller.SessionParams params =
                    new PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL);
            params.setAppPackageName(ctx.getPackageName());

            int sessionId = installer.createSession(params);
            try (PackageInstaller.Session session = installer.openSession(sessionId)) {
                try (OutputStream out = session.openWrite("magpie.apk", 0, apk.length());
                     InputStream in = new FileInputStream(apk)) {
                    byte[] buffer = new byte[64 * 1024];
                    int n;
                    while ((n = in.read(buffer)) > 0) {
                        out.write(buffer, 0, n);
                    }
                    session.fsync(out);
                }
                Intent callback = new Intent(ctx, InstallReceiver.class)
                        .setAction(InstallReceiver.ACTION_RESULT);
                PendingIntent pending = PendingIntent.getBroadcast(
                        ctx,
                        sessionId,
                        callback,
                        PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_MUTABLE);
                session.commit(pending.getIntentSender());
            }
        } catch (Exception e) {
            ShellUi.toast(ctx, ctx.getString(R.string.update_failed, String.valueOf(e.getMessage())));
        }
    }
}
