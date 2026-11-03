package az.manydesk;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.graphics.PixelFormat;
import android.hardware.display.DisplayManager;
import android.hardware.display.VirtualDisplay;
import android.media.Image;
import android.media.ImageReader;
import android.media.projection.MediaProjection;
import android.media.projection.MediaProjectionManager;
import android.os.Handler;
import android.os.HandlerThread;
import android.os.IBinder;
import android.util.DisplayMetrics;

import java.nio.ByteBuffer;

/**
 * Sharing this phone's screen: MediaProjection mirrored into an ImageReader, every new
 * picture handed to Rust (android.rs, ScreenSource). A foreground service, as Android
 * requires, with the usual "ManyDesk is sharing your screen" notification.
 */
public class ScreenCapture extends Service {
    static native void nativeFrame(ByteBuffer pixels, int width, int height, int rowStride);
    static native void nativeStopped();

    private MediaProjection projection;
    private VirtualDisplay display;
    private ImageReader reader;
    private HandlerThread thread;

    @Override
    public IBinder onBind(Intent i) {
        return null;
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int id) {
        NotificationManager nm = getSystemService(NotificationManager.class);
        nm.createNotificationChannel(new NotificationChannel("share", "Screen sharing", NotificationManager.IMPORTANCE_LOW));
        Notification n = new Notification.Builder(this, "share")
                .setContentTitle("ManyDesk")
                .setContentText("Your screen is being shared")
                .setSmallIcon(android.R.drawable.ic_menu_view)
                .setOngoing(true)
                .build();
        startForeground(1, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION);

        MediaProjectionManager mpm = getSystemService(MediaProjectionManager.class);
        Intent data = intent.getParcelableExtra("data");
        projection = mpm.getMediaProjection(intent.getIntExtra("code", 0), data);
        if (projection == null) {
            stopSelf();
            return START_NOT_STICKY;
        }
        thread = new HandlerThread("capture");
        thread.start();
        Handler h = new Handler(thread.getLooper());
        projection.registerCallback(new MediaProjection.Callback() {
            @Override
            public void onStop() {
                nativeStopped();
                stopSelf();
            }
        }, h);

        DisplayMetrics m = getResources().getDisplayMetrics();
        android.view.WindowManager wm = getSystemService(android.view.WindowManager.class);
        DisplayMetrics real = new DisplayMetrics();
        wm.getDefaultDisplay().getRealMetrics(real);
        int w = real.widthPixels, hgt = real.heightPixels;
        reader = ImageReader.newInstance(w, hgt, PixelFormat.RGBA_8888, 2);
        reader.setOnImageAvailableListener(r -> {
            Image img = r.acquireLatestImage();
            if (img == null) return;
            try {
                Image.Plane p = img.getPlanes()[0];
                nativeFrame(p.getBuffer(), img.getWidth(), img.getHeight(), p.getRowStride());
            } finally {
                img.close();
            }
        }, h);
        display = projection.createVirtualDisplay("ManyDesk", w, hgt, m.densityDpi,
                DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR, reader.getSurface(), null, h);
        return START_NOT_STICKY;
    }

    @Override
    public void onDestroy() {
        if (display != null) display.release();
        if (projection != null) projection.stop();
        if (reader != null) reader.close();
        if (thread != null) thread.quitSafely();
        super.onDestroy();
    }
}
