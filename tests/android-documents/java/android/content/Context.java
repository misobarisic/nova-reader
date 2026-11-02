package android.content;
import java.io.File;
public class Context {
    public static final String CLIPBOARD_SERVICE="clipboard";
    public static final int MODE_PRIVATE=0;
    public static final String NOTIFICATION_SERVICE="notification", POWER_SERVICE="power";
    public static final android.app.NotificationManager notifications=new android.app.NotificationManager();
    public static final android.os.PowerManager power=new android.os.PowerManager();
    public static boolean notificationAvailable=true,powerAvailable=true;
    public static volatile int foregroundStarts,ordinaryStarts,serviceStops,classLoads;
    public static volatile boolean failForegroundStartOnce;
    public static Context serviceContext;
    public static Intent serviceIntent;
    public android.content.pm.PackageManager getPackageManager() {return new android.content.pm.PackageManager();}
    public ComponentName startForegroundService(Intent intent) {foregroundStarts++;serviceIntent=intent;serviceContext=this;if(failForegroundStartOnce){failForegroundStartOnce=false;throw new IllegalStateException("fixture foreground start rejected");}return new ComponentName();}
    public ComponentName startService(Intent intent) {ordinaryStarts++;serviceIntent=intent;return new ComponentName();}
    public boolean stopService(Intent intent) {serviceStops++;serviceIntent=intent;serviceContext=this;return false;}

    private static final java.util.Map<String,MemoryPreferences> PREFERENCES=new java.util.HashMap<>();
    public String getPackageName() { return "dev.misob.nova.reader"; }
    public SharedPreferences getSharedPreferences(String name,int mode) { return PREFERENCES.computeIfAbsent(name,key -> new MemoryPreferences()); }
    private static final class MemoryPreferences implements SharedPreferences {
        private final java.util.Map<String,String> values=new java.util.HashMap<>();
        public String getString(String key,String fallback) { return values.getOrDefault(key,fallback); }
        public Editor edit() { return new Editor() {
            private final java.util.Map<String,String> changes=new java.util.HashMap<>();
            public Editor putString(String key,String value) { changes.put(key,value);return this; }
            public Editor remove(String key) { changes.put(key,null);return this; }
            public boolean commit() { apply();return true; }
            public void apply() { for (java.util.Map.Entry<String,String> entry:changes.entrySet()) {if(entry.getValue()==null) values.remove(entry.getKey());else values.put(entry.getKey(),entry.getValue());} }
        }; }
    }
    private static final ContentResolver RESOLVER=new ContentResolver();
    private static final ClipboardManager CLIPBOARD=new ClipboardManager();
    public static Intent browserIntent;
    public static Context browserContext;
    public static int browserLaunches;
    public static boolean failBrowserLaunch;
    public ClassLoader getClassLoader() { classLoads++;return getClass().getClassLoader(); }
    public File getCacheDir() { return new File(System.getProperty("java.io.tmpdir")); }
    public Context getApplicationContext() { return this; }
    public ContentResolver getContentResolver() { return RESOLVER; }
    public Object getSystemService(String name) {
        if(NOTIFICATION_SERVICE.equals(name))return notificationAvailable?notifications:null;
        if(POWER_SERVICE.equals(name))return powerAvailable?power:null;
        return CLIPBOARD_SERVICE.equals(name)?CLIPBOARD:null;
    }
    public void startActivity(Intent intent) {
        if (failBrowserLaunch) throw new IllegalStateException("fixture no browser activity");
        if ((intent.getFlags()&Intent.FLAG_ACTIVITY_NEW_TASK)==0) throw new AssertionError("Application browser launch needs NEW_TASK");
        browserIntent=intent;browserContext=this;browserLaunches++;
    }
}
