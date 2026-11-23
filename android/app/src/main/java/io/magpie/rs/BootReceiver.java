package io.magpie.rs;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

/**
 * Starts the gateway after a reboot (or after this very APK was replaced by an
 * in-app update), when the user turned autostart on.
 *
 * <p>It reads {@code shell.json} directly instead of asking the native library,
 * so a boot with no other app code loaded still works.
 */
public final class BootReceiver extends BroadcastReceiver {

    @Override
    public void onReceive(Context ctx, Intent intent) {
        String action = intent == null ? null : intent.getAction();
        if (!Intent.ACTION_BOOT_COMPLETED.equals(action)
                && !Intent.ACTION_MY_PACKAGE_REPLACED.equals(action)
                && !"android.intent.action.QUICKBOOT_POWERON".equals(action)) {
            return;
        }
        if (ShellPrefs.autostart(ctx)) {
            MagpieService.start(ctx);
        }
    }
}
