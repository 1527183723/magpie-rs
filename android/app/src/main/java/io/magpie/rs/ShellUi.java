package io.magpie.rs;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.Context;
import android.widget.ProgressBar;

/**
 * The three pieces of UI that cannot live in a WebView: the update dialog, the
 * download progress, and the toast used for one-off messages.
 *
 * <p>Rust calls these through JNI ({@link #showUpdate}, {@link #updateProgress})
 * and gets the answer back through {@code MainActivity.onUpdateChoice}.
 */
public final class ShellUi {

    private static AlertDialog progressDialog;
    private static ProgressBar progressBar;

    private ShellUi() {
    }

    private static Activity activity(Context ctx) {
        return ctx instanceof Activity ? (Activity) ctx : null;
    }

    /** Called from Rust: a newer release exists. */
    public static void showUpdate(Context ctx, String version, String size, String notes) {
        Activity a = activity(ctx);
        if (a == null) {
            return;
        }
        StringBuilder message = new StringBuilder();
        if (size != null && !size.isEmpty()) {
            message.append(size).append('\n');
        }
        if (notes != null && !notes.isEmpty()) {
            message.append('\n').append(notes);
        }
        a.runOnUiThread(() -> new AlertDialog.Builder(a)
                .setTitle(a.getString(R.string.update_title, version))
                .setMessage(message.toString().trim())
                .setPositiveButton(R.string.update_now, (d, w) -> choice(a, 1))
                .setNegativeButton(R.string.update_later, (d, w) -> choice(a, 2))
                .setNeutralButton(R.string.update_skip, (d, w) -> choice(a, 3))
                .show());
    }

    /** Called from Rust while the APK downloads. */
    public static void updateProgress(Context ctx, int percent) {
        Activity a = activity(ctx);
        if (a == null) {
            return;
        }
        a.runOnUiThread(() -> {
            if (progressDialog == null) {
                progressBar = new ProgressBar(a, null, android.R.attr.progressBarStyleHorizontal);
                progressBar.setMax(100);
                progressBar.setProgress(0);
                progressBar.setPadding(48, 32, 48, 32);
                progressDialog = new AlertDialog.Builder(a)
                        .setTitle(R.string.update_downloading)
                        .setView(progressBar)
                        .setCancelable(false)
                        .show();
            }
            if (progressBar != null) {
                progressBar.setProgress(percent);
            }
            if (percent >= 100 && progressDialog != null) {
                progressDialog.dismiss();
                progressDialog = null;
                progressBar = null;
            }
        });
    }

    /** One-off message; safe to call from any thread Rust happens to be on. */
    public static void toast(Context ctx, String message) {
        Activity a = activity(ctx);
        if (a == null) {
            return;
        }
        a.runOnUiThread(() ->
                android.widget.Toast.makeText(a, message, android.widget.Toast.LENGTH_LONG).show());
    }

    private static void choice(Activity a, int which) {
        if (a instanceof MainActivity main) {
            main.onUpdateChoice(which);
        }
    }
}
