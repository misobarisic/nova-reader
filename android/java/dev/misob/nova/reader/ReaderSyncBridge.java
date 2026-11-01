package dev.misob.nova.reader;

import android.content.Context;
import android.net.ConnectivityManager;
import android.net.Network;
import android.util.Log;

/** Application-only connectivity observer; never launches an Activity or service. */
public final class ReaderSyncBridge {
    private static ConnectivityManager manager;
    private static ConnectivityManager.NetworkCallback callback;
    private ReaderSyncBridge() {}
    public static synchronized void schedule(Context context, boolean enabled) {
        if (enabled) ReaderSyncJobService.schedule(context);
        else ReaderSyncJobService.cancel(context);
    }
    public static synchronized void observeNetwork(Context context, boolean enabled) {
        if (enabled && callback == null) {
            manager = (ConnectivityManager) context.getApplicationContext()
                    .getSystemService(Context.CONNECTIVITY_SERVICE);
            if (manager == null) return;
            ConnectivityManager.NetworkCallback observer = new ConnectivityManager.NetworkCallback() {
                @Override public void onAvailable(Network network) { nativeNetworkChanged(); }
                @Override public void onLost(Network network) { nativeNetworkChanged(); }
            };
            try {
                manager.registerDefaultNetworkCallback(observer);
                callback = observer;
            } catch (RuntimeException error) {
                Log.w("ReaderSync", "network observer unavailable", error);
                manager = null;
            }
        } else if (!enabled && callback != null) {
            try { manager.unregisterNetworkCallback(callback); }
            catch (RuntimeException error) { Log.w("ReaderSync", "network observer cleanup", error); }
            callback = null;
            manager = null;
        }
    }
    public static void copyText(Context context, String text) {
        android.content.ClipboardManager clipboard = (android.content.ClipboardManager)
                context.getSystemService(Context.CLIPBOARD_SERVICE);
        if (clipboard == null) throw new IllegalStateException("Clipboard unavailable");
        clipboard.setPrimaryClip(android.content.ClipData.newPlainText("Nova Reader invite", text));
    }
    private static native void nativeNetworkChanged();
}
