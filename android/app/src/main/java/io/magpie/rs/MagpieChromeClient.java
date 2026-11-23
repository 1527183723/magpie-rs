package io.magpie.rs;

import android.content.Context;
import android.os.Message;
import android.webkit.WebChromeClient;
import android.webkit.WebView;
import android.webkit.WebViewClient;

/**
 * Handles {@code window.open}: the page's plugin sign-in buttons and every
 * "open the provider's site" link end up here.
 */
final class MagpieChromeClient extends WebChromeClient {

    private final Context context;

    MagpieChromeClient(Context context) {
        this.context = context.getApplicationContext();
    }

    @Override
    public boolean onCreateWindow(WebView view, boolean isDialog, boolean isUserGesture, Message resultMsg) {
        // A WebView cannot host a popup without a window manager, so a throwaway
        // WebView is created purely to learn the URL the page wanted, and that
        // URL goes to the browser.
        WebView probe = new WebView(view.getContext());
        probe.setWebViewClient(new WebViewClient() {
            @Override
            public boolean shouldOverrideUrlLoading(WebView v, android.webkit.WebResourceRequest request) {
                MagpieWebClient.openExternal(context, request.getUrl().toString());
                v.destroy();
                return true;
            }

            @Override
            @SuppressWarnings("deprecation")
            public boolean shouldOverrideUrlLoading(WebView v, String url) {
                MagpieWebClient.openExternal(context, url);
                v.destroy();
                return true;
            }
        });
        if (resultMsg != null && resultMsg.obj instanceof WebView.WebViewTransport transport) {
            transport.setWebView(probe);
            resultMsg.sendToTarget();
            return true;
        }
        probe.destroy();
        return false;
    }
}
