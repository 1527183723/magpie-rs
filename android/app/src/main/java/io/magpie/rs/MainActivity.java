package io.magpie.rs;

import android.app.Activity;
import android.content.pm.PackageInfo;
import android.os.Build;
import android.os.Bundle;
import android.view.ViewGroup;
import android.webkit.WebSettings;
import android.webkit.WebView;

/**
 * The only Activity in the app.
 *
 * <p>It creates one {@link WebView}, hands it to Rust, and gets out of the way.
 * Every decision after that point — which URL loads, when it reloads, whether an
 * update is offered — is made in {@code libmagpie.so}. The Java here exists
 * because Android instantiates Activities itself; there is no JNI-free way to
 * declare one.
 */
public final class MainActivity extends Activity {

    static {
        System.loadLibrary("magpie");
    }

    private WebView web;

    // Implemented in crates/magpie-android/src/lib.rs.
    private native void nativeStart(String filesDir, String versionName, long versionCode, int sdk);

    private native void nativeAttachWebView(WebView view);

    private native void nativeStop();

    private native String nativeStatus();

    private native void nativeCheckUpdate(boolean silent);

    private native void nativeUpdateChoice(int choice);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        web = new WebView(this);
        WebSettings s = web.getSettings();
        s.setJavaScriptEnabled(true);
        s.setDomStorageEnabled(true);
        // The page is served over loopback http and never needs to read local
        // files or reach arbitrary content:// providers.
        s.setAllowFileAccess(false);
        s.setAllowContentAccess(false);
        s.setMixedContentMode(WebSettings.MIXED_CONTENT_NEVER_ALLOW);
        s.setUseWideViewPort(true);
        s.setLoadWithOverviewMode(true);
        s.setSupportZoom(false);
        s.setBuiltInZoomControls(false);
        s.setTextZoom(100);

        web.setWebViewClient(new MagpieWebClient(this));
        web.setWebChromeClient(new MagpieChromeClient(this));
        web.setBackgroundColor(0xFF0E1116);

        setContentView(web, new ViewGroup.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT));

        String versionName = "1.0";
        long versionCode = 1;
        try {
            PackageInfo info = getPackageManager().getPackageInfo(getPackageName(), 0);
            if (info.versionName != null) {
                versionName = info.versionName;
            }
            versionCode = Build.VERSION.SDK_INT >= 28 ? info.getLongVersionCode() : info.versionCode;
        } catch (Exception ignored) {
            // A missing PackageInfo means the manager is still starting up; the
            // defaults above are only used for the version banner.
        }

        nativeStart(getFilesDir().getAbsolutePath(), versionName, versionCode, Build.VERSION.SDK_INT);
        nativeAttachWebView(web);

        // The foreground service is what keeps the gateway alive once the screen
        // goes off; starting it here is also what the notification's "Open"
        // action lands on.
        MagpieService.start(this);
    }

    @Override
    protected void onResume() {
        super.onResume();
        if (web != null) {
            web.onResume();
        }
    }

    @Override
    protected void onPause() {
        if (web != null) {
            web.onPause();
        }
        super.onPause();
    }

    @Override
    protected void onDestroy() {
        // The gateway is NOT stopped here: leaving the Activity (rotation, back
        // to the launcher) must not take the agents' endpoint down. The service
        // owns the lifetime; its "Stop" action and the notification are the way
        // to end it.
        if (web != null) {
            web.destroy();
            web = null;
        }
        super.onDestroy();
    }

    /** Called by {@link ShellUi} when the user answers the update dialog. */
    void onUpdateChoice(int choice) {
        nativeUpdateChoice(choice);
    }

    /** Used by the service notification and by ShellUi. */
    String statusJson() {
        return nativeStatus();
    }

    void checkUpdate(boolean silent) {
        nativeCheckUpdate(silent);
    }

    void stopGateway() {
        nativeStop();
    }
}
