package io.magpie.rs;

import android.app.Activity;
import android.content.pm.PackageInfo;
import android.os.Build;
import android.os.Bundle;
import android.util.Log;
import android.view.ViewGroup;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.widget.ScrollView;
import android.widget.TextView;

import java.io.PrintWriter;
import java.io.StringWriter;

/**
 * The only Activity in the app.
 *
 * <p>It creates one {@link WebView}, hands it to Rust, and gets out of the way.
 * Every decision after that point — which URL loads, when it reloads, whether an
 * update is offered — is made in {@code libmagpie.so}.
 *
 * <p>Start-up is wrapped in {@code try/catch} on purpose. A phone at the other
 * end of this file has no adb attached, so a thrown exception that reaches the
 * framework is indistinguishable from "the app is broken" with nothing to act
 * on. Instead, whatever went wrong is rendered on screen: device, ABI, WebView
 * version, and the stack trace.
 */
public final class MainActivity extends Activity {

    private static final String TAG = "magpie";

    /** Set when the native library refuses to load; checked before anything else. */
    private static volatile Throwable libError;

    static {
        try {
            System.loadLibrary("magpie");
        } catch (Throwable t) {
            // Not rethrown: an ExceptionInInitializerError here would take the
            // Activity down before any UI exists.
            libError = t;
        }
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

        if (libError != null) {
            showFailure("native library did not load", libError);
            return;
        }
        try {
            buildUi();
        } catch (Throwable t) {
            showFailure("start-up failed", t);
        }
    }

    private void buildUi() {
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
            // Only used for the version banner.
        }

        nativeStart(getFilesDir().getAbsolutePath(), versionName, versionCode, Build.VERSION.SDK_INT);
        nativeAttachWebView(web);

        // The foreground service is what keeps the gateway alive once the screen
        // goes off; starting it here is also what the notification's "Open"
        // action lands on. A refusal here must not take the page down with it.
        try {
            MagpieService.start(this);
        } catch (Throwable t) {
            android.util.Log.w("magpie", "could not start the foreground service", t);
        }
    }

    /** Render whatever failed, with enough context to act on it. */
    private void showFailure(String what, Throwable t) {
        StringBuilder sb = new StringBuilder();
        sb.append("magpie ").append(versionNameOf()).append('\n');
        sb.append("Android ").append(Build.VERSION.RELEASE)
                .append(" (API ").append(Build.VERSION.SDK_INT).append(")\n");
        sb.append("ABI: ").append(abi()).append('\n');
        sb.append("WebView: ").append(webViewVersion()).append('\n');
        sb.append("package: ").append(getPackageName()).append("\n\n");
        sb.append(what).append("\n\n");

        StringWriter sw = new StringWriter();
        t.printStackTrace(new PrintWriter(sw));
        sb.append(sw);

        TextView tv = new TextView(this);
        tv.setText(sb.toString());
        tv.setTextSize(11);
        tv.setTextIsSelectable(true);
        tv.setPadding(32, 72, 32, 32);
        tv.setBackgroundColor(0xFF0E1116);
        tv.setTextColor(0xFFE6EDF3);

        ScrollView sv = new ScrollView(this);
        sv.setBackgroundColor(0xFF0E1116);
        sv.addView(tv);
        setContentView(sv);
    }

    private String versionNameOf() {
        try {
            PackageInfo info = getPackageManager().getPackageInfo(getPackageName(), 0);
            return info.versionName == null ? "?" : info.versionName;
        } catch (Exception e) {
            return "?";
        }
    }

    private static String abi() {
        String[] abis = Build.SUPPORTED_ABIS;
        return abis != null && abis.length > 0 ? abis[0] : "?";
    }

    private String webViewVersion() {
        try {
            PackageInfo info = getPackageManager().getPackageInfo("com.google.android.webview", 0);
            return info.versionName == null ? "present" : info.versionName;
        } catch (Exception e) {
            try {
                PackageInfo info = WebView.getCurrentWebViewPackage();
                return info == null ? "unknown" : String.valueOf(info.versionName);
            } catch (Throwable t) {
                return "unavailable (" + e.getClass().getSimpleName() + ")";
            }
        }
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

    // ---- entry points for Rust (called from any thread) -------------------

    /** Rust: show the page. {@code loadUrl} must run on the main Looper. */
    public void magpieLoadUrl(String url) {
        final WebView w = web;
        if (w == null) {
            Log.w(TAG, "magpieLoadUrl before the WebView existed");
            return;
        }
        runOnUiThread(() -> {
            try {
                w.loadUrl(url);
            } catch (Throwable t) {
                Log.w(TAG, "loadUrl failed", t);
            }
        });
    }

    /** Rust: render a self-contained failure page. */
    public void magpieLoadHtml(String html) {
        final WebView w = web;
        if (w == null) {
            Log.w(TAG, "magpieLoadHtml before the WebView existed");
            return;
        }
        runOnUiThread(() -> {
            try {
                w.loadDataWithBaseURL(null, html, "text/html", "utf-8", null);
            } catch (Throwable t) {
                Log.w(TAG, "loadDataWithBaseURL failed", t);
            }
        });
    }

    public void magpieToast(String message) {
        runOnUiThread(() -> ShellUi.toast(this, message));
    }

    public void magpieShowUpdate(String version, String size, String notes) {
        runOnUiThread(() -> ShellUi.showUpdate(this, version, size, notes));
    }

    public void magpieUpdateProgress(int percent) {
        runOnUiThread(() -> ShellUi.updateProgress(this, percent));
    }

    /** Copying an APK is disk I/O: keep it off the main thread. */
    public void magpieInstall(String path) {
        Thread worker = new Thread(() -> ApkInstaller.install(this, path), "magpie-install");
        worker.setDaemon(true);
        worker.start();
    }
    /** Called by {@link ShellUi} when the user answers the update dialog. */
    void onUpdateChoice(int choice) {
        try {
            nativeUpdateChoice(choice);
        } catch (Throwable t) {
            android.util.Log.w("magpie", "update choice failed", t);
        }
    }

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
