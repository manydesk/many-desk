package az.manydesk;

import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.Intent;
import android.provider.Settings;
import android.view.View;
import android.view.inputmethod.InputMethodManager;

import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

/** What the Rust side (src/android.rs) calls; everything static, any thread. */
public final class Bridge {
    static volatile MainActivity activity;

    static native void nativeDisplay(int width, int height, float density);

    /** Texts the system shows for ManyDesk (notifications), in the app's language: set by
     *  the Rust side (android.rs, push_texts) before they are needed. */
    private static final java.util.HashMap<String, String> texts = new java.util.HashMap<>();

    public static void setTexts(String[] kv) {
        synchronized (texts) {
            for (int i = 0; i + 1 < kv.length; i += 2) texts.put(kv[i], kv[i + 1]);
        }
    }

    static String text(String key, String fallback) {
        synchronized (texts) {
            String s = texts.get(key);
            return s == null || s.isEmpty() ? fallback : s;
        }
    }
    static native void nativeInsets(int top, int bottom, int left, int right);

    // ------------------------------------------------------------ clipboard ---

    public static void setClip(final String text) {
        final MainActivity a = activity;
        if (a == null) return;
        a.runOnUiThread(() -> {
            ClipboardManager cm = (ClipboardManager) a.getSystemService(Context.CLIPBOARD_SERVICE);
            cm.setPrimaryClip(ClipData.newPlainText("ManyDesk", text));
        });
    }

    /** Set by the clipboard listener (MainActivity): read the clipboard only after a change —
     *  Android tells the user "ManyDesk pasted from your clipboard" on every read. */
    static volatile boolean clipChanged = true;

    /** The clipboard's text after a change, else null (Android only lets the app in front read it). */
    public static String getClip() {
        final MainActivity a = activity;
        if (a == null || !clipChanged) return null;
        final String[] out = new String[1];
        final CountDownLatch done = new CountDownLatch(1);
        a.runOnUiThread(() -> {
            try {
                ClipboardManager cm = (ClipboardManager) a.getSystemService(Context.CLIPBOARD_SERVICE);
                ClipData c = cm.getPrimaryClip();
                if (c != null && c.getItemCount() > 0) {
                    CharSequence t = c.getItemAt(0).coerceToText(a);
                    out[0] = t == null ? null : t.toString();
                }
                clipChanged = false; // read (a refusal throws and leaves it for next time)
            } catch (Exception ignored) {
            } finally {
                done.countDown();
            }
        });
        try {
            done.await(500, TimeUnit.MILLISECONDS);
        } catch (InterruptedException ignored) {
        }
        return out[0];
    }

    // ------------------------------------------------------- soft keyboard ---

    public static void keyboard(final boolean show) {
        final MainActivity a = activity;
        if (a == null) return;
        a.runOnUiThread(() -> {
            View v = a.getWindow().getDecorView();
            InputMethodManager imm = (InputMethodManager) a.getSystemService(Context.INPUT_METHOD_SERVICE);
            if (show) {
                v.requestFocus();
                imm.showSoftInput(v, InputMethodManager.SHOW_IMPLICIT);
            } else {
                imm.hideSoftInputFromWindow(v.getWindowToken(), 0);
            }
        });
    }

    // ------------------------------------------------- sharing this screen ---

    public static void startCapture() {
        final MainActivity a = activity;
        if (a == null) return;
        a.runOnUiThread(a::askCapture);
    }

    public static void stopCapture() {
        final MainActivity a = activity;
        if (a == null) return;
        a.stopService(new Intent(a, ScreenCapture.class));
    }

    // -------------------------------------------------------------- update ---

    /** The installer's answer when it is not a success (InstallReceiver). */
    static native void nativeUpdateFailed(boolean cancelled, String message);

    /**
     * Installs a downloaded update (an APK in the app's own folder, already checked against
     * the release's SHA256SUMS). The system shows its confirmation; the new version replaces
     * this one and the app opens again. Returns null when the installer took it, "allow" when
     * the user has to let ManyDesk install apps first (that setting is opened), or the error.
     */
    public static String installApk(String path) {
        final MainActivity a = activity;
        if (a == null) return "no window";
        if (!a.getPackageManager().canRequestPackageInstalls()) {
            a.startActivity(new Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, android.net.Uri.parse("package:" + a.getPackageName()))
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
            return "allow";
        }
        try {
            java.io.File f = new java.io.File(path);
            android.content.pm.PackageInstaller pi = a.getPackageManager().getPackageInstaller();
            android.content.pm.PackageInstaller.SessionParams p = new android.content.pm.PackageInstaller.SessionParams(
                    android.content.pm.PackageInstaller.SessionParams.MODE_FULL_INSTALL);
            p.setAppPackageName(a.getPackageName());
            p.setSize(f.length());
            int id = pi.createSession(p);
            try (android.content.pm.PackageInstaller.Session s = pi.openSession(id)) {
                try (java.io.OutputStream out = s.openWrite("base.apk", 0, f.length());
                     java.io.InputStream in = new java.io.FileInputStream(f)) {
                    byte[] buf = new byte[1 << 16];
                    for (int n; (n = in.read(buf)) > 0; ) out.write(buf, 0, n);
                    s.fsync(out);
                }
                int flags = android.app.PendingIntent.FLAG_UPDATE_CURRENT;
                if (android.os.Build.VERSION.SDK_INT >= 31) flags |= android.app.PendingIntent.FLAG_MUTABLE;
                android.app.PendingIntent done = android.app.PendingIntent.getBroadcast(a, id, new Intent(a, InstallReceiver.class), flags);
                s.commit(done.getIntentSender());
            }
            return null;
        } catch (Throwable e) {
            return String.valueOf(e.getMessage());
        }
    }

    // ----------------------------------------------------------- wallpaper ---

    /**
     * The home-screen wallpaper as a JPEG at most maxWidth wide (the viewer's recent tile),
     * or null. Android 13 and later show it to apps only with "All files access".
     */
    public static byte[] wallpaper(int maxWidth) {
        final MainActivity a = activity;
        if (a == null) return null;
        try {
            android.graphics.drawable.Drawable d = android.app.WallpaperManager.getInstance(a).getDrawable();
            if (d == null) return null;
            int w = d.getIntrinsicWidth(), h = d.getIntrinsicHeight();
            if (w <= 0 || h <= 0) {
                android.util.DisplayMetrics m = a.getResources().getDisplayMetrics();
                w = m.widthPixels;
                h = m.heightPixels;
            }
            float s = Math.min(1f, maxWidth / (float) w);
            int bw = Math.max(1, Math.round(w * s)), bh = Math.max(1, Math.round(h * s));
            android.graphics.Bitmap b = android.graphics.Bitmap.createBitmap(bw, bh, android.graphics.Bitmap.Config.ARGB_8888);
            d.setBounds(0, 0, bw, bh);
            d.draw(new android.graphics.Canvas(b));
            java.io.ByteArrayOutputStream out = new java.io.ByteArrayOutputStream();
            b.compress(android.graphics.Bitmap.CompressFormat.JPEG, 80, out);
            b.recycle();
            return out.toByteArray();
        } catch (Throwable e) {
            return null;
        }
    }

    // --------------------------------------------------------------- files ---

    /** "All files access" (Android 11+) or the storage permission (older). */
    public static boolean filesAccess() {
        if (android.os.Build.VERSION.SDK_INT >= 30) return android.os.Environment.isExternalStorageManager();
        final MainActivity a = activity;
        return a != null && a.checkSelfPermission("android.permission.WRITE_EXTERNAL_STORAGE") == android.content.pm.PackageManager.PERMISSION_GRANTED;
    }

    public static void openFilesAccess() {
        final MainActivity a = activity;
        if (a == null) return;
        if (android.os.Build.VERSION.SDK_INT >= 30) {
            Intent i = new Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
                    android.net.Uri.parse("package:" + a.getPackageName()));
            i.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            try {
                a.startActivity(i);
            } catch (Exception e) {
                a.startActivity(new Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
            }
        } else {
            a.runOnUiThread(() -> a.requestPermissions(new String[] {
                    "android.permission.READ_EXTERNAL_STORAGE", "android.permission.WRITE_EXTERNAL_STORAGE"}, 3));
        }
    }

    // ------------------------------------------------------------ requests ---

    /** A connection request while ManyDesk is out of sight: a notification that opens it. */
    public static void notifyRequest(final String name) {
        final MainActivity a = activity;
        if (a == null) return;
        android.app.NotificationManager nm = a.getSystemService(android.app.NotificationManager.class);
        if (name == null || a.visible) {
            nm.cancel(2);
            return;
        }
        nm.createNotificationChannel(new android.app.NotificationChannel("requests", text("n.reqChannel", "Connection requests"),
                android.app.NotificationManager.IMPORTANCE_HIGH));
        Intent open = new Intent(a, MainActivity.class).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP);
        android.app.PendingIntent pi = android.app.PendingIntent.getActivity(a, 0, open,
                android.app.PendingIntent.FLAG_IMMUTABLE | android.app.PendingIntent.FLAG_UPDATE_CURRENT);
        android.app.Notification n = new android.app.Notification.Builder(a, "requests")
                .setContentTitle(name)
                .setContentText(text("n.reqText", "wants to connect to this phone — tap to answer"))
                .setSmallIcon(android.R.drawable.ic_menu_view)
                .setCategory(android.app.Notification.CATEGORY_CALL)
                .setContentIntent(pi)
                .setAutoCancel(true)
                .build();
        nm.notify(2, n);
    }

    // -------------------------------------------------------------- camera ---

    public static void startCamera() {
        CameraCapture.start();
    }

    public static void stopCamera() {
        CameraCapture.stop();
    }

    // ------------------------------------------------------ remote control ---

    public static void inject(String json) {
        InputService s = InputService.instance;
        if (s != null) s.handle(json);
        else android.util.Log.w("ManyDesk", "input dropped: remote control is off in Settings → Accessibility");
    }

    public static boolean inputReady() {
        return InputService.instance != null;
    }

    /** ManyDesk's App info page: its ⋮ menu has "Allow restricted settings", which Android 13+
     *  (Samsung, Xiaomi…) asks for before an installed-by-hand app may use accessibility. */
    public static void openAppInfo() {
        final MainActivity a = activity;
        if (a == null) return;
        Intent i = new Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, android.net.Uri.parse("package:" + a.getPackageName()));
        i.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        a.startActivity(i);
    }

    /** Opens Settings → Accessibility, where the user turns ManyDesk remote control on. */
    public static void openInputSettings() {
        final MainActivity a = activity;
        if (a == null) return;
        Intent i = new Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS);
        i.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        a.startActivity(i);
    }
}
