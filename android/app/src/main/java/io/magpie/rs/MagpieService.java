package io.magpie.rs;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.os.IBinder;
import android.util.Log;

/**
 * Keeps the gateway alive once the screen goes off.
 *
 * <p>The gateway itself runs inside this process — there is no child binary to
 * supervise. Android still needs a foreground service to promise the process
 * will not be frozen, so this class exists for the notification and nothing
 * else.
 */
public final class MagpieService extends Service {

    private static final String TAG = "magpie";

    /** Set when the native library refuses to load. */
    private static volatile Throwable libError;

    // BootReceiver can start this service without MainActivity ever having run,
    // and a failure here would otherwise kill the process silently.
    static {
        try {
            System.loadLibrary("magpie");
        } catch (Throwable t) {
            libError = t;
            Log.e(TAG, "cannot load libmagpie.so", t);
        }
    }

    static final String ACTION_START = "io.magpie.rs.action.START";
    static final String ACTION_STOP = "io.magpie.rs.action.STOP";

    private static final String CHANNEL = "magpie";
    private static final int NOTIFICATION_ID = 1;

    private native void nativeEnsureRunning(String filesDir);

    private native void nativeStop();

    private native boolean nativeRunning();

    static void start(Context ctx) {
        Intent intent = new Intent(ctx, MagpieService.class).setAction(ACTION_START);
        ctx.startForegroundService(intent);
    }

    static void stop(Context ctx) {
        Intent intent = new Intent(ctx, MagpieService.class).setAction(ACTION_STOP);
        try {
            ctx.startService(intent);
        } catch (IllegalStateException e) {
            // Background start restricted: the process is going away anyway, and
            // the gateway dies with it.
            Log.w(TAG, "stop action refused", e);
        }
    }

    @Override
    public void onCreate() {
        super.onCreate();
        channel(this);
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        String action = intent == null ? ACTION_START : intent.getAction();
        if (ACTION_STOP.equals(action)) {
            if (libError == null) {
                try {
                    nativeStop();
                } catch (Throwable t) {
                    Log.w(TAG, "nativeStop failed", t);
                }
            }
            stopForeground(STOP_FOREGROUND_REMOVE);
            stopSelf();
            return START_NOT_STICKY;
        }

        try {
            startForeground(NOTIFICATION_ID, notification(this));
        } catch (Throwable t) {
            // A refused foreground start is a platform decision, not a reason to
            // crash the process that hosts the gateway.
            Log.w(TAG, "startForeground refused", t);
        }

        if (libError == null) {
            try {
                nativeEnsureRunning(getFilesDir().getAbsolutePath());
            } catch (Throwable t) {
                Log.e(TAG, "gateway did not start", t);
            }
        }
        return START_STICKY;
    }

    @Override
    public void onDestroy() {
        // Not nativeStop(): a sticky restart must find the gateway still up if the
        // process outlives the service object.
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    private static void channel(Context ctx) {
        NotificationManager nm = ctx.getSystemService(NotificationManager.class);
        if (nm == null) {
            return;
        }
        NotificationChannel channel = new NotificationChannel(
                CHANNEL,
                ctx.getString(R.string.notification_channel),
                NotificationManager.IMPORTANCE_LOW);
        channel.setDescription(ctx.getString(R.string.notification_channel_desc));
        channel.setShowBadge(false);
        nm.createNotificationChannel(channel);
    }

    private Notification notification(Context ctx) {
        int flags = PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE;
        PendingIntent open = PendingIntent.getActivity(
                ctx, 0, new Intent(ctx, MainActivity.class), flags);
        PendingIntent stop = PendingIntent.getService(
                ctx, 1, new Intent(ctx, MagpieService.class).setAction(ACTION_STOP), flags);

        int port = ShellPrefs.gatewayPort(ctx);
        int webPort = ShellPrefs.webPort(ctx);
        boolean lan = ShellPrefs.lan(ctx);
        String where = (lan ? "0.0.0.0:" : "127.0.0.1:") + port;

        return new Notification.Builder(ctx, CHANNEL)
                .setSmallIcon(R.drawable.ic_stat_magpie)
                .setContentTitle(ctx.getString(R.string.notification_title))
                .setContentText(ctx.getString(R.string.notification_text, where, "127.0.0.1:" + webPort))
                .setContentIntent(open)
                .setOngoing(true)
                .setForegroundServiceBehavior(Notification.FOREGROUND_SERVICE_IMMEDIATE)
                .addAction(new Notification.Action.Builder(null,
                        ctx.getString(R.string.notification_open), open).build())
                .addAction(new Notification.Action.Builder(null,
                        ctx.getString(R.string.notification_stop), stop).build())
                .build();
    }
}
