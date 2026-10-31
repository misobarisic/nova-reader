package dev.misob.nova.reader;
import android.app.Activity;
import android.content.ClipboardManager;
import android.content.ContentResolver;
import android.content.Intent;
import android.net.Uri;
import android.provider.MediaStore;
import android.os.Bundle;
import android.os.Build;
import android.os.Handler;
import android.view.View;
import android.view.Window;
import android.view.WindowInsets;
import android.view.WindowManager;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileNotFoundException;
import java.io.RandomAccessFile;
import java.lang.reflect.Field;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
/** Runs the production activity against a controlled main-thread queue. */
public class DocumentBridgeFixture {
    private static ReaderActivity current;
    private static ReaderActivity previous;
    public static void create() { current=new ReaderActivity(); current.onCreate(new Bundle()); restartProvider(); }
    public static void restartProvider() {
        ReaderImageProvider provider=new ReaderImageProvider();
        provider.attachInfo(new android.app.Application(),null);
        ContentResolver.provider=provider;
    }
    public static void recreate() {
        previous=current;
        previous.changing=true;
        create();
        previous.onDestroy();
    }
    public static void destroy() { current.onDestroy(); current=null; }
    public static void drain() { Handler.drain(); }
    public static void cancelResult() { current.onActivityResult(Activity.launchedRequest,0,null); }
    public static String action() { return Activity.launchedIntent.action; }
    public static boolean currentLaunched() { return Activity.launchedBy==current; }
    public static String mime() { return Activity.launchedIntent.type; }
    public static String title() { return Activity.launchedIntent.title; }
    public static void exportResult() {
        current.onActivityResult(Activity.launchedRequest,Activity.RESULT_OK,new Intent("fixture").setData(Uri.parse("content://fixture/export")));
    }
    public static void awaitCopies() throws Exception {
        Field field=ReaderActivity.class.getDeclaredField("COPIES");field.setAccessible(true);
        ((ExecutorService)field.get(null)).submit(() -> {}).get(5,TimeUnit.SECONDS);
    }
    public static String exported() { return new String(ContentResolver.exported.toByteArray(),StandardCharsets.UTF_8); }
    public static void resetMedia() { ContentResolver.resetMedia(); }
    public static void failMedia(int phase) {
        ContentResolver.failInsert=phase==1;ContentResolver.failOutput=phase==2;
        ContentResolver.failWrite=phase==3;ContentResolver.failClose=phase==4;ContentResolver.failCommit=phase==5;
    }
    public static int mediaRows() { return ContentResolver.mediaRows.size(); }
    public static int mediaDeletes() { return ContentResolver.deletes; }
    public static int mediaCommits() { return ContentResolver.commits; }
    public static String mediaPath() { return ContentResolver.lastMedia.getAsString(MediaStore.Images.Media.RELATIVE_PATH); }
    public static String mediaName() { return ContentResolver.lastMedia.getAsString(MediaStore.Images.Media.DISPLAY_NAME); }
    public static String mediaMime() { return ContentResolver.lastMedia.getAsString(MediaStore.Images.Media.MIME_TYPE); }
    public static boolean mediaAllVisible() {
        for (android.content.ContentValues row:ContentResolver.mediaRows.values()) {
            if (!Integer.valueOf(0).equals(row.getAsInteger(MediaStore.Images.Media.IS_PENDING))) return false;
        }
        return true;
    }
    public static void cancelMediaDuringWrite() {
        ContentResolver.duringWrite=() -> {
            try {
                Field active=ReaderActivity.class.getDeclaredField("active");active.setAccessible(true);
                Object request=active.get(null);
                Field id=request.getClass().getDeclaredField("id");id.setAccessible(true);
                ReaderActivity.cancelDocument(id.getLong(request));
            } catch (Exception error) {throw new AssertionError(error);}
        };
    }
    public static String clipboardUri() { return ClipboardManager.primary.uri.toString(); }
    public static String clipboardMime() { return ClipboardManager.primary.mime; }
    public static String clipboardBytes() throws Exception {
        try (android.os.ParcelFileDescriptor descriptor=ContentResolver.provider.openFile(ClipboardManager.primary.uri,"r");
             FileInputStream input=new FileInputStream(descriptor.getFileDescriptor())) {
            byte[] bytes=new byte[128];int count=input.read(bytes);
            return new String(bytes,0,count,StandardCharsets.UTF_8);
        }
    }
    public static boolean providerRejectsUnsafePaths() throws Exception {
        String name=ClipboardManager.primary.uri.getPathSegments().get(0);
        for (String text:new String[]{"file:///"+name,"content://other/"+name,"content://dev.misob.nova.reader.images/../"+name,"content://dev.misob.nova.reader.images/%2e%2e/"+name,"content://dev.misob.nova.reader.images/"+name+"/extra"}) {
            try {ContentResolver.provider.openFile(Uri.parse(text),"r");return false;}
            catch (FileNotFoundException expected) { }
        }
        try {ContentResolver.provider.openFile(ClipboardManager.primary.uri,"w");return false;}
        catch (FileNotFoundException expected) { }
        try (android.database.Cursor cursor=ContentResolver.provider.query(ClipboardManager.primary.uri,null,null,null,null)) {
            if (!cursor.moveToFirst() || !"Reader image.png".equals(cursor.getString(0)) || cursor.getLong(1)<=0) return false;
        }
        return true;
    }
    public static boolean retentionAndBounds() throws Exception {
        android.content.Context context=new android.app.Application();
        File source=new File(context.getCacheDir(),"provider-source.png");
        try (RandomAccessFile input=new RandomAccessFile(source,"rw")) {input.setLength(8);}
        File latest=null;
        for (int i=0;i<10;i++) latest=ReaderImageProvider.stage(context,source,"image/png",new AtomicBoolean());
        File directory=latest.getParentFile();
        if (directory.listFiles().length>8) return false;
        if (!latest.setLastModified(System.currentTimeMillis()-25L*60*60*1000)) return false;
        if (ContentResolver.provider.getType(ReaderImageProvider.uriFor(latest))!=null) return false;
        ReaderImageProvider.stage(context,source,"image/png",new AtomicBoolean());
        if (latest.exists()) return false;
        int count=directory.listFiles().length;
        try {ReaderImageProvider.stage(context,source,"image/png",new AtomicBoolean(true));return false;}
        catch (java.io.IOException expected) { }
        if (directory.listFiles().length>count) return false;
        try (RandomAccessFile input=new RandomAccessFile(source,"rw")) {input.setLength(33L*1024*1024);}
        try {ReaderImageProvider.stage(context,source,"image/png",new AtomicBoolean());return false;}
        catch (java.io.IOException expected) { }
        source.delete();return directory.listFiles().length<=8;
    }
    public static void setApi(int api) { Build.VERSION.SDK_INT=api; }
    private static boolean statusHidden(ReaderActivity activity) {
        Window window=activity.getWindow();
        return Build.VERSION.SDK_INT>=30
            ? (window.getInsetsController().hiddenTypes&WindowInsets.Type.statusBars())!=0
            : (window.getDecorView().getSystemUiVisibility()&View.SYSTEM_UI_FLAG_FULLSCREEN)!=0;
    }
    public static boolean statusHidden() { return statusHidden(current); }
    public static boolean previousStatusHidden() { return statusHidden(previous); }
    private static int windowUpdates(ReaderActivity activity) {
        return activity.getWindow().getInsetsController().updates+activity.getWindow().getDecorView().updates;
    }
    public static int windowUpdates() { return windowUpdates(current); }
    public static int previousWindowUpdates() { return windowUpdates(previous); }
    public static boolean navigationHidden() {
        Window window=current.getWindow();
        return Build.VERSION.SDK_INT>=30
            ? (window.getInsetsController().hiddenTypes&WindowInsets.Type.navigationBars())!=0
            : (window.getDecorView().getSystemUiVisibility()&View.SYSTEM_UI_FLAG_HIDE_NAVIGATION)!=0;
    }
    public static boolean transientSwipe() {
        return Build.VERSION.SDK_INT>=30
            ? current.getWindow().getInsetsController().behavior==2
            : (current.getWindow().getDecorView().getSystemUiVisibility()&View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY)!=0;
    }
    public static boolean stableLayout() {
        return Build.VERSION.SDK_INT>=30?!current.getWindow().decorFitsSystemWindows
            :(current.getWindow().getDecorView().getSystemUiVisibility()&View.SYSTEM_UI_FLAG_LAYOUT_STABLE)!=0;
    }
    public static boolean edgeToEdge() {
        int layout=View.SYSTEM_UI_FLAG_LAYOUT_STABLE|View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION|View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN;
        Window window=current.getWindow();
        return Build.VERSION.SDK_INT>=30?!window.decorFitsSystemWindows
            :(window.getDecorView().getSystemUiVisibility()&layout)==layout;
    }
    public static boolean transparentStatusBar() {
        Window window=current.getWindow();
        return window.statusBarColor==0
            && (window.flags()&WindowManager.LayoutParams.FLAG_DRAWS_SYSTEM_BAR_BACKGROUNDS)!=0
            && (window.flags()&(WindowManager.LayoutParams.FLAG_TRANSLUCENT_STATUS|WindowManager.LayoutParams.FLAG_FULLSCREEN))==0;
    }
    public static boolean cutoutAllowed() {
        Window window=current.getWindow();
        if (Build.VERSION.SDK_INT<28) return window.cutoutAccesses==0;
        return window.getAttributes().layoutInDisplayCutoutMode==(Build.VERSION.SDK_INT>=30
            ?WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS
            :WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES);
    }
    public static boolean unrelatedWindowFlagsPreserved() {
        Window window=current.getWindow();
        return (window.flags()&WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)!=0
            && (window.getDecorView().getSystemUiVisibility()&View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR)!=0;
    }
    public static int layoutChanges() {
        Window window=current.getWindow();
        return Build.VERSION.SDK_INT>=30?window.decorFitChanges:window.getDecorView().layoutChanges;
    }
    public static int decorFitCalls() { return current.getWindow().decorFitCalls; }
    public static void deferInsets() { Window.initiallyInsetsReady=false; }
    public static void attachInsets() { current.getWindow().insetsReady=true;Window.initiallyInsetsReady=true; }
    public static void disturbWindowLayout() {
        Window window=current.getWindow();
        if (Build.VERSION.SDK_INT>=30) window.setDecorFitsSystemWindows(true);
        else window.getDecorView().setSystemUiVisibility(window.getDecorView().getSystemUiVisibility()
            &~(View.SYSTEM_UI_FLAG_LAYOUT_STABLE|View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION|View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN));
        window.setStatusBarColor(0xff000000);
        if (Build.VERSION.SDK_INT>=28) window.getAttributes().layoutInDisplayCutoutMode=0;
    }
    public static void revealStatusBar() {
        Window window=current.getWindow();
        if (Build.VERSION.SDK_INT>=30) window.getInsetsController().show(WindowInsets.Type.statusBars());
        else window.getDecorView().setSystemUiVisibility(window.getDecorView().getSystemUiVisibility()&~View.SYSTEM_UI_FLAG_FULLSCREEN);
    }
    public static void focus() { current.onWindowFocusChanged(true); }
}
