package io.magpie.rs;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageInstaller;
import android.widget.Toast;

/** Reports how the {@link PackageInstaller} session ended. */
public final class InstallReceiver extends BroadcastReceiver {

    static final String ACTION_RESULT = "io.magpie.rs.INSTALL_RESULT";

    @Override
    public void onReceive(Context ctx, Intent intent) {
        if (intent == null || !ACTION_RESULT.equals(intent.getAction())) {
            return;
        }
        int status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE);
        if (status == PackageInstaller.STATUS_PENDING_USER_ACTION) {
            Intent confirm = intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent.class);
            if (confirm != null) {
                confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
                ctx.startActivity(confirm);
            }
            return;
        }
        if (status != PackageInstaller.STATUS_SUCCESS) {
            String message = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE);
            Toast.makeText(ctx, ctx.getString(R.string.update_failed, String.valueOf(message)),
                    Toast.LENGTH_LONG).show();
        }
    }
}
