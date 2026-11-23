package io.magpie.rs;

import android.app.Activity;
import android.util.Log;
import android.webkit.WebView;
import android.widget.Toast;

import java.util.function.Consumer;

/**
 * Every WebView call from Rust goes through here.
 *
 * <p>{@code WebView} enforces that all of its methods run on the Looper thread
 * that created it — here the main thread. A JNI caller has no Looper at all, so
 * calling {@code loadUrl} straight from a Rust thread throws
 * {@code "A WebView method was called on thread 'Thread-N'"} and kills the
 * process, because nothing on that thread catches it.
 *
 * <p>{@link Activity#runOnUiThread} is the hop; it is safe from any thread and
 * works even before the WebView is attached to a window.
 */
public final class WebOps {

    private static final String TAG = "magpie";

    private WebOps() {
    }

    public static void loadUrl(Activity activity, WebView view, String url) {
        post(activity, view, v -> v.loadUrl(url));
    }

    public static void loadHtml(Activity activity, WebView view, String html) {
        post(activity, view,
                v -> v.loadDataWithBaseURL(null, html, "text/html", "utf-8", null));
    }

    public static void reload(Activity activity, WebView view) {
        post(activity, view, WebView::reload);
    }

    public static void toast(Activity activity, String message) {
        if (activity == null || message == null) {
            return;
        }
        Runnable r = () -> {
            try {
                Toast.makeText(activity, message, Toast.LENGTH_LONG).show();
            } catch (Throwable t) {
                Log.w(TAG, "toast failed", t);
            }
        };
        activity.runOnUiThread(r);
    }

    private static void post(Activity activity, WebView view, Consumer<WebView> action) {
        if (view == null) {
            return;
        }
        Runnable r = () -> {
            try {
                action.accept(view);
            } catch (Throwable t) {
                Log.w(TAG, "webview operation failed", t);
            }
        };
        if (activity != null) {
            activity.runOnUiThread(r);
        } else {
            view.post(r);
        }
    }
}
