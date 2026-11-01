package android.app;
import android.content.Intent;
import android.os.Bundle;
import android.view.Window;
public class Activity extends android.content.Context {
    public static final int RESULT_OK=-1;
    public static Activity launchedBy;
    public static Intent launchedIntent;
    public static int launchedRequest;
    public boolean changing;
    private boolean destroyed;
    private int requestedOrientation=-1;
    private final Window window=new Window();
    public int getRequestedOrientation() { return requestedOrientation; }
    public void setRequestedOrientation(int orientation) { requestedOrientation=orientation; }
    protected void onPause() { }
    protected void onResume() { }
    public Window getWindow() { return window; }
    public void onWindowFocusChanged(boolean hasFocus) { }
    public boolean isFinishing() { return false; }
    public boolean isDestroyed() { return destroyed; }
    public boolean isChangingConfigurations() { return changing; }
    public void runOnUiThread(Runnable runnable) { runnable.run(); }
    public void startActivityForResult(Intent intent, int request) {
        launchedBy=this; launchedIntent=intent; launchedRequest=request;
    }
    public void finishActivity(int request) { }
    protected void onCreate(Bundle state) { }
    protected void onDestroy() { destroyed=true; }
    protected void onActivityResult(int code,int result,Intent data) { }
}
