package android.view;
public class WindowInsetsController {
    public static final int BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE=2;
    public int hiddenTypes;
    public int behavior;
    public int updates;
    public void hide(int types) { hiddenTypes|=types; updates++; }
    public void show(int types) { hiddenTypes&=~types; updates++; }
    public void setSystemBarsBehavior(int behavior) { this.behavior=behavior; }
}
