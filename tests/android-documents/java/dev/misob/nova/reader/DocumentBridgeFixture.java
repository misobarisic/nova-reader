package dev.misob.nova.reader;
import android.app.Activity;
import android.os.Bundle;
import android.os.Handler;
/** Runs the production activity against a controlled main-thread queue. */
public class DocumentBridgeFixture {
    private static ReaderActivity current;
    public static void create() { current=new ReaderActivity(); current.onCreate(new Bundle()); }
    public static void recreate() {
        ReaderActivity previous=current;
        previous.changing=true;
        create();
        previous.onDestroy();
    }
    public static void destroy() { current.onDestroy(); current=null; }
    public static void drain() { Handler.drain(); }
    public static void cancelResult() { current.onActivityResult(Activity.launchedRequest,0,null); }
    public static String action() { return Activity.launchedIntent.action; }
    public static boolean currentLaunched() { return Activity.launchedBy==current; }
}
