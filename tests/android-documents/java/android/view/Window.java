package android.view;
public class Window {
    private final View decor=new View();
    private final WindowInsetsController controller=new WindowInsetsController();
    private WindowManager.LayoutParams attributes=new WindowManager.LayoutParams();
    public boolean decorFitsSystemWindows=true;
    public static boolean initiallyInsetsReady=true;
    public boolean insetsReady=initiallyInsetsReady;
    public int decorFitCalls;
    public int decorFitChanges;
    public int cutoutAccesses;
    public int statusBarColor=0xff000000;
    public View getDecorView() { return decor; }
    public WindowInsetsController getInsetsController() { return insetsReady?controller:null; }
    public void setDecorFitsSystemWindows(boolean fits) {
        if (android.os.Build.VERSION.SDK_INT<30) throw new AssertionError("Decor fitting requires API 30");
        if (decorFitsSystemWindows!=fits) decorFitChanges++;
        decorFitCalls++;
        decorFitsSystemWindows=fits;
    }
    public void setStatusBarColor(int color) { statusBarColor=color; }
    public void addFlags(int flags) { attributes.flags|=flags; }
    public void clearFlags(int flags) { attributes.flags&=~flags; }
    public int flags() { return attributes.flags; }
    public WindowManager.LayoutParams getAttributes() {
        if (android.os.Build.VERSION.SDK_INT>=28) cutoutAccesses++;
        return attributes;
    }
    public void setAttributes(WindowManager.LayoutParams attributes) { this.attributes=attributes; }
}
