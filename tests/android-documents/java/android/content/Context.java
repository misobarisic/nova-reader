package android.content;
import java.io.File;
public class Context {
    public static final String CLIPBOARD_SERVICE="clipboard";
    private static final ContentResolver RESOLVER=new ContentResolver();
    private static final ClipboardManager CLIPBOARD=new ClipboardManager();
    public ClassLoader getClassLoader() { return getClass().getClassLoader(); }
    public File getCacheDir() { return new File(System.getProperty("java.io.tmpdir")); }
    public Context getApplicationContext() { return this; }
    public ContentResolver getContentResolver() { return RESOLVER; }
    public Object getSystemService(String name) { return CLIPBOARD_SERVICE.equals(name)?CLIPBOARD:null; }
}
