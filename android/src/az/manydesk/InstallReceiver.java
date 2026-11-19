package az.manydesk;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageInstaller;

/**
 * The system installer's answer to an update (Bridge.installApk). It first asks for the
 * user's confirmation: that screen is opened here. A failure goes back to the Rust side.
 */
public class InstallReceiver extends BroadcastReceiver {
    @Override
    public void onReceive(Context c, Intent i) {
        int status = i.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE);
        if (status == PackageInstaller.STATUS_PENDING_USER_ACTION) {
            Intent confirm = i.getParcelableExtra(Intent.EXTRA_INTENT);
            if (confirm != null) {
                confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
                c.startActivity(confirm);
            }
            return;
        }
        if (status != PackageInstaller.STATUS_SUCCESS) {
            String msg = i.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE);
            Bridge.nativeUpdateFailed(status == PackageInstaller.STATUS_FAILURE_ABORTED, msg == null ? "" : msg);
        }
    }
}
