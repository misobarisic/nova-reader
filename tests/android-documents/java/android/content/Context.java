package android.content;
import java.io.File;
public class Context {
    public static final String CLIPBOARD_SERVICE="clipboard";
    private static final ContentResolver RESOLVER=new ContentResolver();
    private static final ClipboardManager CLIPBOARD=new ClipboardManager();
    public static Intent browserIntent;
    public static Context browserContext;
    public static int browserLaunches;
    public static boolean failBrowserLaunch;
    public ClassLoader getClassLoader() { return getClass().getClassLoader(); }
    public File getCacheDir() { return new File(System.getProperty("java.io.tmpdir")); }
    public Context getApplicationContext() { return this; }
    public ContentResolver getContentResolver() { return RESOLVER; }
    public Object getSystemService(String name) { return CLIPBOARD_SERVICE.equals(name)?CLIPBOARD:null; }
    public void startActivity(Intent intent) {
        if (failBrowserLaunch) throw new IllegalStateException("fixture no browser activity");
        if ((intent.getFlags()&Intent.FLAG_ACTIVITY_NEW_TASK)==0) throw new AssertionError("Application browser launch needs NEW_TASK");
        browserIntent=intent;browserContext=this;browserLaunches++;
    }
}
