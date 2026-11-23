package io.magpie.rs;

import android.content.ActivityNotFoundException;
import android.content.Context;
import android.content.Intent;
import android.net.Uri;
import android.webkit.WebResourceRequest;
import android.webkit.WebView;
import android.webkit.WebViewClient;

/**
 * Keeps the page on loopback and hands everything else to a real browser.
 *
 * <p>Plugin sign-in happens through {@code window.open(url, "_blank")}; a plain
 * WebView drops that request, which makes the button look dead. Intercepting the
 * navigation and opening the system browser is what the Go shell did in
 * {@code onCreateWindow}, and the same trick applies here.
 */
final class MagpieWebClient extends WebViewClient {

    private final Context context;

    MagpieWebClient(Context context) {
        this.context = context.getApplicationContext();
    }

    static boolean isLocal(String url) {
        if (url == null) {
            return false;
        }
        Uri uri = Uri.parse(url);
        String host = uri.getHost();
        if (host == null) {
            return true; // about:blank, data:, javascript: — let the WebView have it
        }
        return "127.0.0.1".equals(host)
                || "localhost".equals(host)
                || "[::1]".equals(host)
                || "::1".equals(host);
    }

    static void openExternal(Context context, String url) {
        try {
            Intent intent = new Intent(Intent.ACTION_VIEW, Uri.parse(url));
            intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            context.startActivity(intent);
        } catch (ActivityNotFoundException ignored) {
            // No browser: nothing useful to say, and a toast would be noise.
        }
    }

    @Override
    public boolean shouldOverrideUrlLoading(WebView view, WebResourceRequest request) {
        String url = request.getUrl().toString();
        if (isLocal(url)) {
            return false;
        }
        openExternal(context, url);
        return true;
    }

    @Override
    @SuppressWarnings("deprecation")
    public boolean shouldOverrideUrlLoading(WebView view, String url) {
        if (isLocal(url)) {
            return false;
        }
        openExternal(context, url);
        return true;
    }
}
