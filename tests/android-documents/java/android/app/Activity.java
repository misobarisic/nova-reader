package android.app;
import android.content.Intent;
import android.os.Bundle;
import android.view.Window;
public class Activity extends android.content.Context {
    public static final int RESULT_OK=-1;
    private static final Application APPLICATION=new Application();
    @Override public android.content.Context getApplicationContext() {return APPLICATION;}
    public static Activity launchedBy;
    public static Intent launchedIntent;
    public static int launchedRequest;
    public boolean changing,finishing,permissionGranted;
    public int permissionRequests,permissionCode;
    public String[] requestedPermissions;
    public String permissionThread;
    public int checkSelfPermission(String permission) {return permissionGranted?0:-1;}
    public void requestPermissions(String[] permissions,int code) {permissionRequests++;permissionCode=code;requestedPermissions=permissions;permissionThread=Thread.currentThread().getName();}

    private boolean destroyed;
    private int requestedOrientation=-1;
    private final Window window=new Window();
    public int getRequestedOrientation() { return requestedOrientation; }
    public void setRequestedOrientation(int orientation) { requestedOrientation=orientation; }
    protected void onPause() { }
    protected void onResume() { }
    public Window getWindow() { return window; }
    public void onWindowFocusChanged(boolean hasFocus) { }
    public boolean isFinishing() { return finishing; }
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
