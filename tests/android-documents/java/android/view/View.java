package android.view;
public class View {
    public static final int SYSTEM_UI_FLAG_FULLSCREEN=4;
    public static final int SYSTEM_UI_FLAG_HIDE_NAVIGATION=2;
    public static final int SYSTEM_UI_FLAG_LAYOUT_STABLE=256;
    public static final int SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN=1024;
    public static final int SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION=512;
    public static final int SYSTEM_UI_FLAG_LIGHT_STATUS_BAR=8192;
    public static final int SYSTEM_UI_FLAG_IMMERSIVE_STICKY=4096;
    private int flags=SYSTEM_UI_FLAG_LIGHT_STATUS_BAR|SYSTEM_UI_FLAG_FULLSCREEN|SYSTEM_UI_FLAG_HIDE_NAVIGATION;
    public int updates;
    public int layoutChanges;
    public int getSystemUiVisibility() { return flags; }
    public void setSystemUiVisibility(int flags) {
        int layout=SYSTEM_UI_FLAG_LAYOUT_STABLE|SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION|SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN;
        if ((this.flags&layout)!=(flags&layout)) layoutChanges++;
        this.flags=flags;updates++;
    }
    public boolean post(Runnable runnable) { return new android.os.Handler(android.os.Looper.getMainLooper()).post(runnable); }
}
