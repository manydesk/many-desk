package az.manydesk;

import android.app.NativeActivity;
import android.content.Intent;
import android.media.projection.MediaProjectionManager;
import android.os.Build;
import android.os.Bundle;
import android.util.DisplayMetrics;

/**
 * The app: a NativeActivity running the Rust/egui UI (libmanydesk.so, android_main).
 * Besides that it only answers what needs an Activity: the screen-capture consent.
 */
public class MainActivity extends NativeActivity {
    static final int REQ_CAPTURE = 1;
    /** In front (resumed): requests are answered in the app itself, no notification. */
    volatile boolean visible;

    @Override
    protected void onResume() {
        super.onResume();
        visible = true;
        getSystemService(android.app.NotificationManager.class).cancel(2);
    }

    @Override
    protected void onPause() {
        visible = false;
        super.onPause();
    }

    static {
        // Loaded here too (NativeActivity dlopens it as well — same library) so the JNI
        // methods of Bridge and ScreenCapture resolve.
        System.loadLibrary("manydesk");
    }

    @Override
    protected void onCreate(Bundle state) {
        Bridge.activity = this;
        reportDisplay();
        if (Build.VERSION.SDK_INT >= 33) {
            requestPermissions(new String[] {"android.permission.POST_NOTIFICATIONS"}, 2);
        }
        super.onCreate(state);
        ((android.content.ClipboardManager) getSystemService(CLIPBOARD_SERVICE))
                .addPrimaryClipChangedListener(() -> Bridge.clipChanged = true);
        // dark icons on the light bars
        getWindow().getDecorView().setSystemUiVisibility(android.view.View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR
                | android.view.View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR);
        getWindow().setStatusBarColor(0xFFFFFFFF);
        getWindow().setNavigationBarColor(0xFFFFFFFF);
        // the status and navigation bars: the UI keeps clear of them (android.rs, insets)
        getWindow().getDecorView().setOnApplyWindowInsetsListener((v, in) -> {
            Bridge.nativeInsets(in.getSystemWindowInsetTop(), in.getSystemWindowInsetBottom(),
                    in.getSystemWindowInsetLeft(), in.getSystemWindowInsetRight());
            return v.onApplyWindowInsets(in);
        });
    }

    @Override
    public void onConfigurationChanged(android.content.res.Configuration c) {
        super.onConfigurationChanged(c);
        reportDisplay();
    }

    void reportDisplay() {
        DisplayMetrics m = new DisplayMetrics();
        getWindowManager().getDefaultDisplay().getRealMetrics(m);
        Bridge.nativeDisplay(m.widthPixels, m.heightPixels, m.density);
    }

    void askCapture() {
        MediaProjectionManager mpm = (MediaProjectionManager) getSystemService(MEDIA_PROJECTION_SERVICE);
        startActivityForResult(mpm.createScreenCaptureIntent(), REQ_CAPTURE);
    }

    @Override
    public void onRequestPermissionsResult(int req, String[] perms, int[] results) {
        super.onRequestPermissionsResult(req, perms, results);
        if (req == CameraCapture.REQ_CAMERA) {
            CameraCapture.permission = results.length > 0 && results[0] == android.content.pm.PackageManager.PERMISSION_GRANTED;
        }
    }

    @Override
    protected void onActivityResult(int req, int result, Intent data) {
        super.onActivityResult(req, result, data);
        if (req != REQ_CAPTURE) return;
        if (result != RESULT_OK || data == null) {
            ScreenCapture.nativeStopped();
            return;
        }
        Intent svc = new Intent(this, ScreenCapture.class);
        svc.putExtra("code", result);
        svc.putExtra("data", data);
        startForegroundService(svc);
    }
}
