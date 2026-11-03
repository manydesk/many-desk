package az.manydesk;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;

import java.io.File;

/**
 * Runs in a process of its own (":restart"). MainActivity starts it when it is created in
 * a process that already ran the UI once (winit's event loop cannot start twice): this
 * waits until that process is gone, opens ManyDesk again in a fresh one and leaves.
 */
public class RestartActivity extends Activity {
    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        final int pid = getIntent().getIntExtra("pid", 0);
        final Handler h = new Handler(Looper.getMainLooper());
        final long until = System.currentTimeMillis() + 3000;
        h.post(new Runnable() {
            @Override
            public void run() {
                if (pid != 0 && new File("/proc/" + pid).exists() && System.currentTimeMillis() < until) {
                    h.postDelayed(this, 50);
                    return;
                }
                startActivity(new Intent(RestartActivity.this, MainActivity.class).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
                finish();
                overridePendingTransition(0, 0);
                h.postDelayed(() -> android.os.Process.killProcess(android.os.Process.myPid()), 500);
            }
        });
    }
}
