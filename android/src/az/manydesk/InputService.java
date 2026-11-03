package az.manydesk;

import android.accessibilityservice.AccessibilityService;
import android.accessibilityservice.GestureDescription;
import android.graphics.Path;
import android.graphics.PointF;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.SystemClock;
import android.util.Log;
import android.view.accessibility.AccessibilityEvent;
import android.view.accessibility.AccessibilityNodeInfo;

import org.json.JSONObject;

import java.util.ArrayList;

/**
 * Remote control of this phone. The viewer sends mouse and keyboard events (input.rs, in
 * this screen's pixels); Android only lets an accessibility service act on other apps, so
 * they are played here: a press, its moves and the release become one gesture (a tap, a
 * long press or a swipe), the wheel a short swipe, the right button "back", keys and text
 * edit the focused field. The user turns the service on in Settings → Accessibility.
 *
 * A press and its release often arrive together (a tap on a phone viewer, a slow link):
 * a tap still lasts TAP_MS, as many phones ignore a shorter touch. Wheel steps are summed
 * into one swipe — tiny swipes, one per step, cancel each other or land as taps.
 */
public class InputService extends AccessibilityService {
    static volatile InputService instance;

    private static final long TAP_MS = 60;
    private static final long SCROLL_WAIT_MS = 90;

    private final ArrayList<PointF> stroke = new ArrayList<>();
    private long downAt;
    private float lastX, lastY;
    private final Handler main = new Handler(Looper.getMainLooper());
    private float scrollX, scrollY, scrollDx, scrollDy;
    private final Runnable scrollNow = this::flushScroll;

    @Override
    protected void onServiceConnected() {
        instance = this;
    }

    @Override
    public boolean onUnbind(android.content.Intent i) {
        instance = null;
        return super.onUnbind(i);
    }

    @Override
    public void onAccessibilityEvent(AccessibilityEvent e) {}

    @Override
    public void onInterrupt() {}

    synchronized void handle(String json) {
        if (!json.contains("\"move\"")) Log.d("ManyDesk", "input " + json);
        try {
            JSONObject m = new JSONObject(json);
            String t = m.optString("t");
            float x = (float) m.optDouble("x", lastX), y = (float) m.optDouble("y", lastY);
            switch (t) {
                case "move":
                    lastX = x;
                    lastY = y;
                    if (!stroke.isEmpty()) addPoint(x, y);
                    break;
                case "down":
                    lastX = x;
                    lastY = y;
                    if (m.optInt("b") == 2) {
                        performGlobalAction(GLOBAL_ACTION_BACK);
                        break;
                    }
                    stroke.clear();
                    stroke.add(new PointF(x, y));
                    downAt = SystemClock.uptimeMillis();
                    break;
                case "up":
                    if (stroke.isEmpty()) break;
                    addPoint(x, y);
                    long held = Math.max(TAP_MS, SystemClock.uptimeMillis() - downAt);
                    play(new ArrayList<>(stroke), Math.min(held, 8000));
                    stroke.clear();
                    break;
                case "scroll": {
                    if (scrollDx == 0 && scrollDy == 0) {
                        scrollX = lastX;
                        scrollY = lastY;
                    }
                    scrollDx += (float) m.optDouble("dx", 0);
                    scrollDy += (float) m.optDouble("dy", 0);
                    main.removeCallbacks(scrollNow);
                    main.postDelayed(scrollNow, SCROLL_WAIT_MS);
                    break;
                }
                case "key":
                    if (m.optBoolean("d")) key(m.optString("code"));
                    break;
                case "text":
                    type(m.optString("s"));
                    break;
                case "release":
                    stroke.clear();
                    break;
            }
        } catch (Exception e) {
            Log.w("ManyDesk", "input failed: " + e);
        }
    }

    /** The wheel steps of the last moment as one swipe (wheel down = content up: the finger
     *  moves the other way), kept on the screen and long enough not to be a tap. */
    private synchronized void flushScroll() {
        float dx = scrollDx * 3, dy = scrollDy * 3;
        scrollDx = scrollDy = 0;
        if (Math.abs(dx) + Math.abs(dy) < 24) return;
        android.util.DisplayMetrics d = new android.util.DisplayMetrics();
        ((android.view.WindowManager) getSystemService(WINDOW_SERVICE)).getDefaultDisplay().getRealMetrics(d);
        float maxX = Math.max(1, d.widthPixels - 1), maxY = Math.max(1, d.heightPixels - 1);
        ArrayList<PointF> s = new ArrayList<>();
        s.add(new PointF(Math.min(scrollX, maxX), Math.min(scrollY, maxY)));
        s.add(new PointF(Math.max(1, Math.min(maxX, scrollX - dx)), Math.max(1, Math.min(maxY, scrollY - dy))));
        play(s, 180);
    }

    private void addPoint(float x, float y) {
        PointF last = stroke.get(stroke.size() - 1);
        if (Math.abs(last.x - x) + Math.abs(last.y - y) >= 4 && stroke.size() < 400) {
            stroke.add(new PointF(x, y));
        }
    }

    /** Plays a stroke on the main thread (some phones drop gestures sent from others). */
    private void play(ArrayList<PointF> pts, long ms) {
        main.post(() -> playNow(pts, ms));
    }

    private void playNow(ArrayList<PointF> pts, long ms) {
        Path p = new Path();
        p.moveTo(Math.max(0, pts.get(0).x), Math.max(0, pts.get(0).y));
        for (int i = 1; i < pts.size(); i++) p.lineTo(Math.max(0, pts.get(i).x), Math.max(0, pts.get(i).y));
        if (pts.size() == 1) p.lineTo(Math.max(0, pts.get(0).x), Math.max(0, pts.get(0).y));
        GestureDescription g = new GestureDescription.Builder()
                .addStroke(new GestureDescription.StrokeDescription(p, 0, ms))
                .build();
        boolean ok;
        try {
            ok = dispatchGesture(g, new GestureResultCallback() {
                @Override
                public void onCompleted(GestureDescription d) {
                    Log.d("ManyDesk", "gesture done (" + pts.size() + " points, " + ms + " ms)");
                }

                @Override
                public void onCancelled(GestureDescription d) {
                    Log.w("ManyDesk", "gesture cancelled");
                }
            }, null);
        } catch (RuntimeException e) {
            Log.w("ManyDesk", "gesture failed: " + e);
            return;
        }
        if (!ok) Log.w("ManyDesk", "gesture refused");
    }

    private AccessibilityNodeInfo focused() {
        AccessibilityNodeInfo root = getRootInActiveWindow();
        return root == null ? null : root.findFocus(AccessibilityNodeInfo.FOCUS_INPUT);
    }

    private void setText(AccessibilityNodeInfo n, String s) {
        Bundle b = new Bundle();
        b.putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, s);
        n.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, b);
    }

    private String current(AccessibilityNodeInfo n) {
        // a hint shows as the text of an empty field
        if (n.getText() == null || (Build.VERSION.SDK_INT >= 26 && n.isShowingHintText())) return "";
        return n.getText().toString();
    }

    private void type(String s) {
        AccessibilityNodeInfo n = focused();
        if (n != null && n.isEditable()) setText(n, current(n) + s);
    }

    private void key(String code) {
        switch (code) {
            case "Escape":
                performGlobalAction(GLOBAL_ACTION_BACK);
                return;
            case "Home":
            case "MetaLeft":
            case "MetaRight":
                performGlobalAction(GLOBAL_ACTION_HOME);
                return;
            case "Backspace": {
                AccessibilityNodeInfo n = focused();
                if (n != null && n.isEditable()) {
                    String c = current(n);
                    if (!c.isEmpty()) setText(n, c.substring(0, c.offsetByCodePoints(c.length(), -1)));
                }
                return;
            }
            case "Enter":
            case "NumpadEnter": {
                AccessibilityNodeInfo n = focused();
                if (n != null && Build.VERSION.SDK_INT >= 30) {
                    n.performAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_IME_ENTER.getId());
                }
                return;
            }
            case "Space":
                type(" ");
                return;
            default:
                // letters and digits arrive as "text" in the translated keyboard mode;
                // in the raw mode they are typed here
                if (code.startsWith("Key") && code.length() == 4) type(code.substring(3).toLowerCase());
                else if (code.startsWith("Digit") && code.length() == 6) type(code.substring(5));
        }
    }
}
