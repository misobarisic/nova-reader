package android.content;
import java.io.File;
public class Context {
    public ClassLoader getClassLoader() { return getClass().getClassLoader(); }
    public File getCacheDir() { return new File(System.getProperty("java.io.tmpdir")); }
    public Context getApplicationContext() { return this; }
    public ContentResolver getContentResolver() { throw new UnsupportedOperationException(); }
}
