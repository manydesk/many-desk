package az.manydesk;

import android.content.pm.PackageManager;
import android.graphics.ImageFormat;
import android.hardware.camera2.CameraCaptureSession;
import android.hardware.camera2.CameraCharacteristics;
import android.hardware.camera2.CameraDevice;
import android.hardware.camera2.CameraManager;
import android.hardware.camera2.CaptureRequest;
import android.hardware.camera2.params.StreamConfigurationMap;
import android.media.Image;
import android.media.ImageReader;
import android.os.Handler;
import android.os.HandlerThread;
import android.util.Size;

import java.nio.ByteBuffer;
import java.util.Collections;

/**
 * The session camera (src/media/camera.rs): the front camera through Camera2 into an
 * ImageReader, each picture handed to Rust as I420 with how far it is turned from upright.
 * Asks for the CAMERA permission the first time.
 */
final class CameraCapture {
    static native void nativeFrame(byte[] i420, int width, int height, int rotation);
    static native void nativeError(String reason);

    static final int REQ_CAMERA = 4;
    private static CameraCapture current;
    /** The answer to the permission dialog (MainActivity): null while it is open. */
    static volatile Boolean permission;

    private final MainActivity activity;
    private final HandlerThread thread = new HandlerThread("camera");
    private Handler handler;
    private CameraDevice device;
    private CameraCaptureSession session;
    private ImageReader reader;
    private int sensor;
    private boolean front;
    private volatile boolean stopped;
    private byte[] buf;

    private CameraCapture(MainActivity a) {
        activity = a;
    }

    static synchronized void start() {
        stop();
        MainActivity a = Bridge.activity;
        if (a == null) {
            nativeError("nocamera");
            return;
        }
        current = new CameraCapture(a);
        current.begin();
    }

    static synchronized void stop() {
        if (current != null) {
            current.end();
            current = null;
        }
    }

    private boolean granted() {
        return activity.checkSelfPermission("android.permission.CAMERA") == PackageManager.PERMISSION_GRANTED;
    }

    private void begin() {
        thread.start();
        handler = new Handler(thread.getLooper());
        if (granted()) {
            handler.post(this::open);
            return;
        }
        permission = null;
        activity.runOnUiThread(() -> activity.requestPermissions(new String[] {"android.permission.CAMERA"}, REQ_CAMERA));
        handler.post(new Runnable() {
            int waited;

            @Override
            public void run() {
                if (stopped) return;
                Boolean p = permission;
                if (granted()) {
                    open();
                } else if ((p != null && !p) || waited > 120_000) {
                    nativeError("denied");
                } else {
                    waited += 250;
                    handler.postDelayed(this, 250);
                }
            }
        });
    }

    private void end() {
        stopped = true;
        handler.post(() -> {
            try {
                if (session != null) session.close();
                if (device != null) device.close();
                if (reader != null) reader.close();
            } catch (Exception ignored) {
            }
            thread.quitSafely();
        });
    }

    private void open() {
        if (stopped) return;
        try {
            CameraManager cm = activity.getSystemService(CameraManager.class);
            String id = null;
            for (String c : cm.getCameraIdList()) {
                Integer facing = cm.getCameraCharacteristics(c).get(CameraCharacteristics.LENS_FACING);
                if (facing != null && facing == CameraCharacteristics.LENS_FACING_FRONT) {
                    id = c;
                    break;
                }
                if (id == null) id = c;
            }
            if (id == null) {
                nativeError("nocamera");
                return;
            }
            CameraCharacteristics ch = cm.getCameraCharacteristics(id);
            Integer so = ch.get(CameraCharacteristics.SENSOR_ORIENTATION);
            sensor = so == null ? 0 : so;
            Integer facing = ch.get(CameraCharacteristics.LENS_FACING);
            front = facing != null && facing == CameraCharacteristics.LENS_FACING_FRONT;
            // the size closest to 640×480 (the picture is sent no bigger than that)
            StreamConfigurationMap map = ch.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP);
            Size best = new Size(640, 480);
            long bestScore = Long.MAX_VALUE;
            if (map != null) {
                for (Size s : map.getOutputSizes(ImageFormat.YUV_420_888)) {
                    long score = Math.abs((long) s.getWidth() * s.getHeight() - 640L * 480L);
                    if (s.getWidth() <= 1280 && score < bestScore) {
                        bestScore = score;
                        best = s;
                    }
                }
            }
            reader = ImageReader.newInstance(best.getWidth(), best.getHeight(), ImageFormat.YUV_420_888, 2);
            reader.setOnImageAvailableListener(this::onImage, handler);
            cm.openCamera(id, new CameraDevice.StateCallback() {
                @Override
                public void onOpened(CameraDevice d) {
                    if (stopped) {
                        d.close();
                        return;
                    }
                    device = d;
                    configure();
                }

                @Override
                public void onDisconnected(CameraDevice d) {
                    d.close();
                    if (!stopped) nativeError("camera busy");
                }

                @Override
                public void onError(CameraDevice d, int e) {
                    d.close();
                    if (!stopped) nativeError("camera error " + e);
                }
            }, handler);
        } catch (SecurityException e) {
            nativeError("denied");
        } catch (Exception e) {
            nativeError(String.valueOf(e.getMessage()));
        }
    }

    @SuppressWarnings("deprecation")
    private void configure() {
        try {
            device.createCaptureSession(Collections.singletonList(reader.getSurface()), new CameraCaptureSession.StateCallback() {
                @Override
                public void onConfigured(CameraCaptureSession s) {
                    if (stopped) return;
                    session = s;
                    try {
                        CaptureRequest.Builder b = device.createCaptureRequest(CameraDevice.TEMPLATE_RECORD);
                        b.addTarget(reader.getSurface());
                        s.setRepeatingRequest(b.build(), null, handler);
                    } catch (Exception e) {
                        nativeError(String.valueOf(e.getMessage()));
                    }
                }

                @Override
                public void onConfigureFailed(CameraCaptureSession s) {
                    nativeError("camera busy");
                }
            }, handler);
        } catch (Exception e) {
            nativeError(String.valueOf(e.getMessage()));
        }
    }

    private void onImage(ImageReader r) {
        Image img = r.acquireLatestImage();
        if (img == null) return;
        try {
            if (stopped) return;
            int w = img.getWidth() & ~1, h = img.getHeight() & ~1;
            int size = w * h * 3 / 2;
            if (buf == null || buf.length != size) buf = new byte[size];
            Image.Plane[] p = img.getPlanes();
            copyPlane(p[0], w, h, buf, 0);
            copyPlane(p[1], w / 2, h / 2, buf, w * h);
            copyPlane(p[2], w / 2, h / 2, buf, w * h + w * h / 4);
            // how far the picture is turned from upright, with the phone as it is held now
            int deg = activity.getWindowManager().getDefaultDisplay().getRotation() * 90;
            int rot = front ? (sensor + deg) % 360 : (sensor - deg + 360) % 360;
            nativeFrame(buf, w, h, rot);
        } finally {
            img.close();
        }
    }

    /** One plane into `out` at `at`, tightly packed (rows and pixels may have gaps). */
    private static void copyPlane(Image.Plane plane, int w, int h, byte[] out, int at) {
        ByteBuffer b = plane.getBuffer();
        int rs = plane.getRowStride(), ps = plane.getPixelStride();
        int base = b.position();
        if (ps == 1) {
            for (int y = 0; y < h; y++) {
                b.position(base + y * rs);
                b.get(out, at + y * w, w);
            }
        } else {
            for (int y = 0; y < h; y++) {
                int row = base + y * rs;
                for (int x = 0; x < w; x++) out[at + y * w + x] = b.get(row + x * ps);
            }
        }
    }
}
