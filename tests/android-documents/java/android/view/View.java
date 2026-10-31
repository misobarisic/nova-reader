package android.view;
public class View {
    public static final int SYSTEM_UI_FLAG_FULLSCREEN=4;
    public static final int SYSTEM_UI_FLAG_HIDE_NAVIGATION=2;
    public static final int SYSTEM_UI_FLAG_LAYOUT_STABLE=256;
    public static final int SYSTEM_UI_FLAG_IMMERSIVE_STICKY=4096;
    private int flags=SYSTEM_UI_FLAG_LAYOUT_STABLE;
    public int updates;
    public int getSystemUiVisibility() { return flags; }
    public void setSystemUiVisibility(int flags) { this.flags=flags; updates++; }
}
