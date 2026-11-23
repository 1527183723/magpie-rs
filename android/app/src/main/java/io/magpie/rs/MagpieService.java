package io.magpie.rs;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.os.IBinder;

/**
 * Keeps the gateway alive once the screen goes off.
 *
 * <p>The gateway itself runs inside this process — there is no child binary to
 * supervise any more, which is the one structural difference from the Go shell.
 * Android still needs a foreground service to promise the process will not be
 * frozen, so this class exists for the notification and nothing else.
 */
public final class MagpieService extends Service {

    // The service can be started by BootReceiver without MainActivity ever
    // having run, so its own class initialisation has to load the library.
    static {
        System.loadLibrary("magpie");
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
            android.util.Log.w("magpie", "stop action refused", e);
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
            nativeStop();
            stopForeground(STOP_FOREGROUND_REMOVE);
            stopSelf();
            return START_NOT_STICKY;
        }
        startForeground(NOTIFICATION_ID, notification(this));
        nativeEnsureRunning(getFilesDir().getAbsolutePath());
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
