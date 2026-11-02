package dev.misob.nova.reader;

import android.app.NativeActivity;
import android.content.ContentResolver;
import android.content.ContentUris;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.Intent;
import android.database.Cursor;
import android.graphics.Color;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.Environment;
import android.os.Handler;
import android.os.Looper;
import android.provider.DocumentsContract;
import android.provider.OpenableColumns;
import android.provider.MediaStore;
import android.content.ContentValues;
import android.content.pm.ActivityInfo;
import android.content.pm.PackageManager;
import android.view.View;
import android.view.Window;
import android.view.WindowManager;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.lang.ref.WeakReference;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Locale;
import java.util.Set;
import java.util.UUID;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.atomic.AtomicBoolean;

/** NativeActivity still owns the surface/event loop. SAF copies run off the UI thread. */
public final class ReaderActivity extends NativeActivity {
    private static final long MIB = 1024L * 1024L;
    private static final long PAGE_LIMIT = 32 * MIB;
    private static final long COMIC_LIMIT = 512 * MIB;
    private static final int ENTRY_LIMIT = 10000;
    private static final Object LOCK = new Object();
    private static final ExecutorService COPIES = Executors.newSingleThreadExecutor();
    private static Request active;
    private static long lastCancelledId;
    private static volatile WeakReference<ReaderActivity> foreground = new WeakReference<>(null);
    // Permission UI is owned only by a resumed Activity. Document requests have
    // their separate host lifetime and may survive an Activity pause/recreation.
    private static volatile WeakReference<ReaderActivity> notificationForeground = new WeakReference<>(null);

    public static void requestNotificationPermission(int requestCode) {
        if (Build.VERSION.SDK_INT < 33) return;
        new Handler(Looper.getMainLooper()).post(() -> {
            ReaderActivity activity = notificationForeground.get();
            if (activity == null || activity.isFinishing() || activity.isDestroyed()) return;
            if (activity.checkSelfPermission("android.permission.POST_NOTIFICATIONS")
                    != PackageManager.PERMISSION_GRANTED) {
                activity.requestPermissions(new String[]{"android.permission.POST_NOTIFICATIONS"}, requestCode);
            }
        });
    }

    private static final class ReaderPresentation {
        final boolean reading,immersive,keepAwake,cutout;
        final int rotation;
        ReaderPresentation(boolean reading,boolean immersive,int rotation,boolean keepAwake,boolean cutout) {
            this.reading=reading;this.immersive=immersive;this.rotation=rotation;
            this.keepAwake=keepAwake;this.cutout=cutout;
        }
    }
    private static volatile ReaderPresentation readerPresentation=new ReaderPresentation(false,false,0,false,true);
    private boolean decorFitsConfigured;
    private boolean readerWindowActive;
    private boolean activityResumed;
    private boolean presentationConfigured;
    private int normalOrientation;
    private boolean normalKeepAwake;
    private boolean normalWakePending;
    private int normalCutoutMode;
    private static native void nativeDocumentResult(long id, String path, String name, String error);
    private static native void nativeImageConflict(long id,String name);
    private static native boolean nativeImageSaveCurrent(long id);

    private static final class Request {
        final long id;
        final int kind;
        final String exportPath;
        final String suggestedName;
        final String mime;
        final String seriesFolder;
        final int collisionPolicy;
        final Context application;
        volatile Uri tree;
        volatile boolean selectingTree;
        volatile boolean awaitingChoice;
        volatile ImageStore store;
        volatile ImageTarget target;
        volatile String targetName;
        final AtomicBoolean cancelled = new AtomicBoolean();
        final AtomicBoolean copying = new AtomicBoolean();
        volatile WeakReference<ReaderActivity> activity;
        final File cache;
        final int requestCode;
        Request(long id, int kind, String exportPath, String suggestedName, String mime, String seriesFolder, int collisionPolicy, ReaderActivity activity) {
            this.id=id; this.kind=kind; this.exportPath=exportPath;
            this.suggestedName=suggestedName; this.mime=mime;
            this.seriesFolder=seriesFolder;
            this.collisionPolicy=collisionPolicy;this.application=activity.getApplicationContext();
            this.activity=new WeakReference<>(activity);
            this.cache=activity.getCacheDir();
            this.requestCode=(int)id;
        }
        void check() throws IOException {
            if (cancelled.get() || ((kind==6 || kind==7 || kind==8) && !nativeImageSaveCurrent(id))) throw new IOException("Image action cancelled");
        }
    }
    private static final class ImageDocument {
        final Uri uri; final String name; final long size;
        ImageDocument(Uri uri, String name, long size) {this.uri=uri;this.name=name;this.size=size;}
    }

    public static void setReaderImmersive(boolean immersive) {
        // Compatibility entry point: changes both system bars.
        ReaderPresentation current=readerPresentation;
        readerPresentation=new ReaderPresentation(current.reading,immersive,current.rotation,current.keepAwake,current.cutout);
        postReaderPresentation();
    }
    public static void setReaderPresentation(boolean reading,boolean immersive,int rotation,boolean keepAwake,boolean cutout) {
        if (rotation<0 || rotation>6) throw new IllegalArgumentException("Unknown reader rotation");
        readerPresentation=new ReaderPresentation(reading,reading&&immersive,rotation,keepAwake,cutout);
        postReaderPresentation();
    }
    private static void postReaderPresentation() {
        // Resolve on main and read the latest immutable snapshot, so queued
        // requests cannot affect a destroyed or superseded Activity window.
        boolean posted=new Handler(Looper.getMainLooper()).post(() -> {
            ReaderActivity activity=foreground.get();
            if (activity!=null && !activity.isFinishing() && !activity.isDestroyed()) activity.applyReaderImmersive();
        });
        if (!posted) throw new IllegalStateException("Reader UI thread is unavailable");
    }
    private static int readerOrientation(int rotation) {
        switch(rotation) {
            case 1:return ActivityInfo.SCREEN_ORIENTATION_FULL_SENSOR;
            case 2:return ActivityInfo.SCREEN_ORIENTATION_SENSOR_PORTRAIT;
            case 3:return ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE;
            case 4:return ActivityInfo.SCREEN_ORIENTATION_PORTRAIT;
            case 5:return ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE;
            case 6:return ActivityInfo.SCREEN_ORIENTATION_REVERSE_PORTRAIT;
            default:return ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED;
        }
    }
    @SuppressWarnings("deprecation")
    private void applyReaderWindow(ReaderPresentation state,Window window) {
        if (state.reading && !readerWindowActive) {
            normalOrientation=getRequestedOrientation();
            if (!normalWakePending) normalKeepAwake=(window.getAttributes().flags&WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)!=0;
            if (Build.VERSION.SDK_INT>=28 && presentationConfigured) normalCutoutMode=window.getAttributes().layoutInDisplayCutoutMode;
            readerWindowActive=true;
        }
        if (state.reading) {
            int orientation=readerOrientation(state.rotation);
            if (getRequestedOrientation()!=orientation) setRequestedOrientation(orientation);
            if (activityResumed && state.keepAwake) window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
            else window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        } else if (readerWindowActive) {
            if (getRequestedOrientation()!=normalOrientation) setRequestedOrientation(normalOrientation);
            if (activityResumed && normalKeepAwake) window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
            else window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
            readerWindowActive=false;
        }
    }
    private void applyReaderImmersive() {
        applyReaderImmersive(true);
    }
    @SuppressWarnings("deprecation")
    private void applyReaderImmersive(boolean retryOnAttach) {
        Window window=getWindow();
        ReaderPresentation state=readerPresentation;
        applyReaderWindow(state,window);
        // Slint reports system-bar/cutout insets to the UI. Keep the native
        // surface behind them in both OSD states so showing controls cannot
        // resize the chapter or reserve a black strip above the image.
        window.addFlags(WindowManager.LayoutParams.FLAG_DRAWS_SYSTEM_BAR_BACKGROUNDS);
        window.clearFlags(WindowManager.LayoutParams.FLAG_TRANSLUCENT_STATUS|WindowManager.LayoutParams.FLAG_FULLSCREEN);
        window.setStatusBarColor(Color.TRANSPARENT);
        if (Build.VERSION.SDK_INT>=28) {
            WindowManager.LayoutParams attributes=window.getAttributes();
            int cutoutMode=state.reading
                ?(state.cutout?WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES:WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_NEVER)
                :normalCutoutMode;
            if (attributes.layoutInDisplayCutoutMode!=cutoutMode) {
                attributes.layoutInDisplayCutoutMode=cutoutMode;
                window.setAttributes(attributes);
            }
        }
        if (Build.VERSION.SDK_INT>=30) {
            View decor=window.getDecorView();
            int flags=decor.getSystemUiVisibility();
            int legacyVisibility=View.SYSTEM_UI_FLAG_FULLSCREEN|View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY|View.SYSTEM_UI_FLAG_HIDE_NAVIGATION;
            if ((flags&legacyVisibility)!=0) decor.setSystemUiVisibility(flags&~legacyVisibility);
            // Changing decor fitting redispatches insets; configure once per
            // creation/focus, independently of reader OSD visibility.
            if (!decorFitsConfigured) {
                window.setDecorFitsSystemWindows(false);
                decorFitsConfigured=true;
            }
            WindowInsetsController controller=window.getInsetsController();
            if (controller!=null) {
                // Immersive reading hides the status and navigation bars, as
                // Nova's video player does. A swipe still reveals them
                // transiently; they auto-hide again. Slint's safe-area insets
                // follow the visible bars, so hiding the navigation bar
                // releases its bottom inset instead of leaving a black strip.
                controller.setSystemBarsBehavior(WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);
                if (state.immersive) controller.hide(WindowInsets.Type.systemBars());
                else controller.show(WindowInsets.Type.systemBars());
            } else if (retryOnAttach) {
                // View.post waits for attachment when onCreate has no insets
                // controller yet. Resolve current activity and latest intent;
                // one deferred attempt avoids a loop if attachment never comes.
                decor.post(() -> {
                    ReaderActivity activity=foreground.get();
                    if (activity!=null && !activity.isFinishing() && !activity.isDestroyed()) activity.applyReaderImmersive(false);
                });
            }
        } else {
            View decor=window.getDecorView();
            int flags=(decor.getSystemUiVisibility()&~View.SYSTEM_UI_FLAG_HIDE_NAVIGATION)
                |View.SYSTEM_UI_FLAG_LAYOUT_STABLE|View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION|View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN;
            // API 26–29 immersive-sticky: both bars hidden, swipe reveals.
            int readerFlags=View.SYSTEM_UI_FLAG_FULLSCREEN|View.SYSTEM_UI_FLAG_HIDE_NAVIGATION|View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY;
            decor.setSystemUiVisibility(state.immersive?flags|readerFlags:flags&~readerFlags);
        }
        presentationConfigured=true;
    }
    @Override public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (hasFocus) {decorFitsConfigured=false;applyReaderImmersive();}
    }

    public static void requestDocument(int kind, long id, String exportPath) {
        queueDocument(kind,id,exportPath,"nova-reader-backup.json","application/json","",0);
    }
    public static void requestImage(int kind, long id, String exportPath, String suggestedName, String mime) {
        if (kind!=5 && kind!=6) {nativeDocumentResult(id,null,null,"Unknown image operation");return;}
        queueDocument(kind,id,exportPath,suggestedName,mime,"",0);
    }
    public static void publishImage(long id, String exportPath, String suggestedName, String mime, String seriesFolder) {
        publishImageWithPolicy(id,exportPath,suggestedName,mime,seriesFolder,0);
    }
    public static void publishImageWithPolicy(long id,String exportPath,String suggestedName,String mime,String seriesFolder,int policy) {
        if (policy<0 || policy>2) {nativeDocumentResult(id,null,null,"Unknown image collision policy");return;}
        queueDocument(Build.VERSION.SDK_INT>=29?7:8,id,exportPath,suggestedName,mime,seriesFolder,policy);
    }
    private static void queueDocument(int kind, long id, String exportPath, String suggestedName, String mime, String seriesFolder,int policy) {
        // ndk-context exposes the Application, not an Activity. Resolve the
        // current Reader activity after posting, so recreation cannot leave a
        // queued picker launch targeting the activity that was destroyed.
        boolean posted=new Handler(Looper.getMainLooper()).post(() -> {
            ReaderActivity activity=foreground.get();
            if (activity==null || activity.isFinishing() || activity.isDestroyed()) {
                nativeDocumentResult(id,null,null,"Reader document activity is unavailable"); return;
            }
            activity.openDocument(kind,id,exportPath,suggestedName,mime,seriesFolder,policy);
        });
        if (!posted) nativeDocumentResult(id,null,null,"Reader UI thread is unavailable");
    }
    private void openDocument(int kind, long id, String exportPath, String suggestedName, String mime, String seriesFolder,int policy) {
        if (isFinishing() || isDestroyed()) {
            nativeDocumentResult(id,null,null,"Reader activity is closing"); return;
        }
        if(id<1 || id>65535) {nativeDocumentResult(id,null,null,"Restart the app to open another document");return;}
        Request request=new Request(id,kind,exportPath,suggestedName,mime,seriesFolder,policy,this);
        synchronized (LOCK) {
            if(id<=lastCancelledId) {nativeDocumentResult(id,null,null,"Document selection cancelled");return;}
            if (active!=null) {nativeDocumentResult(id,null,null,"Another document request is active");return;}
            active=request;
        }
        try {
            if (kind==7 || kind==8) {
                validateExport(request);
                ReaderImageProvider.extensionForMime(mime);
                validateSeriesFolder(seriesFolder);
                request.targetName=imageName(suggestedName,mime);
                if (kind==8) {
                    String saved=request.application.getSharedPreferences("reader-image-export",Context.MODE_PRIVATE).getString("tree",null);
                    if (saved==null) {selectImageTree(request);return;}
                    request.tree=Uri.parse(saved);
                    if (!DocumentsContract.isTreeUri(request.tree)) {
                        request.application.getSharedPreferences("reader-image-export",Context.MODE_PRIVATE).edit().remove("tree").apply();
                        selectImageTree(request);return;
                    }
                }
                request.copying.set(true);
                COPIES.execute(() -> prepareImageSave(request));
                return;
            }
            if (kind==6) {
                File original=validateExport(request);
                ReaderImageProvider.extensionForMime(mime);
                request.copying.set(true);
                Context application=getApplicationContext();
                COPIES.execute(() -> copyToClipboard(request,application,original));
                return;
            }
            Intent intent;
            if (kind==2) {
                intent=new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE);
                intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION|Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION);
            } else if (kind==4 || kind==5) {
                validateExport(request);
                if (kind==5) ReaderImageProvider.extensionForMime(mime);
                intent=new Intent(Intent.ACTION_CREATE_DOCUMENT);
                intent.setType(mime);
                intent.addCategory(Intent.CATEGORY_OPENABLE);
                intent.putExtra(Intent.EXTRA_TITLE,kind==5?imageName(suggestedName,mime):"nova-reader-backup.json");
                intent.addFlags(Intent.FLAG_GRANT_WRITE_URI_PERMISSION);
            } else {
                intent=new Intent(Intent.ACTION_OPEN_DOCUMENT);
                intent.addCategory(Intent.CATEGORY_OPENABLE);
                // Providers inconsistently label CBZ and addon ZIP files. Rust
                // validates the actual selected archive, never its MIME label.
                intent.setType("*/*");
                if (kind==3) intent.putExtra(Intent.EXTRA_MIME_TYPES,new String[]{"application/json","text/plain","application/octet-stream"});
                else if (kind==0 || kind==1) intent.putExtra(Intent.EXTRA_MIME_TYPES,new String[]{"application/zip","application/x-zip-compressed","application/x-cbz","application/vnd.comicbook+zip","application/octet-stream"});
                else throw new IOException("Unknown import type");
                intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION|Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION);
            }
            startActivityForResult(intent,request.requestCode);
        } catch (Exception error) {finish(request,null,null,message(error));}
    }
    private void selectImageTree(Request request) {
        request.selectingTree=true;
        Intent intent=new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE);
        intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION|Intent.FLAG_GRANT_WRITE_URI_PERMISSION|Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION|Intent.FLAG_GRANT_PREFIX_URI_PERMISSION);
        startActivityForResult(intent,request.requestCode);
    }
    public static void cancelDocument(long id) {
        Request request;
        synchronized (LOCK) {lastCancelledId=Math.max(lastCancelledId,id);request=active;if(request==null || request.id!=id)return;request.cancelled.set(true);}
        ReaderActivity activity=request.activity.get();
        if (activity!=null) activity.runOnUiThread(() -> {
            if (!request.copying.get()) activity.finishActivity(request.requestCode);
        });
        finish(request,null,null,"Document selection cancelled");
    }
    @Override protected void onActivityResult(int code, int result, Intent data) {
        super.onActivityResult(code,result,data);
        final Request request;
        synchronized (LOCK) {request=active;}
        if (request==null) return;
        if(code!=request.requestCode)return;
        if (request.kind==8 && request.selectingTree) {
            request.selectingTree=false;
            if (result!=RESULT_OK || data==null || data.getData()==null) {finish(request,null,null,"Image action cancelled");return;}
            try {
                request.check();
                Uri tree=data.getData();
                if (!"content".equals(tree.getScheme()) || !DocumentsContract.isTreeUri(tree)) throw new IOException("Unsupported image destination folder");
                int grants=Intent.FLAG_GRANT_READ_URI_PERMISSION|Intent.FLAG_GRANT_WRITE_URI_PERMISSION;
                if ((data.getFlags()&grants)!=grants || (data.getFlags()&Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION)==0) throw new IOException("Choose a folder that supports persistent read/write access");
                request.application.getContentResolver().takePersistableUriPermission(tree,grants);
                if (!request.application.getSharedPreferences("reader-image-export",Context.MODE_PRIVATE).edit().putString("tree",tree.toString()).commit()) throw new IOException("Unable to remember image destination folder");
                request.tree=tree;request.copying.set(true);
                COPIES.execute(() -> prepareImageSave(request));
            } catch (Exception error) {finish(request,null,null,message(error));}
            return;
        }
        if (result!=RESULT_OK || data==null || data.getData()==null) {
            finish(request,null,null,"Document selection cancelled");return;
        }
        Uri uri=data.getData();
        if (!"content".equals(uri.getScheme())) {finish(request,null,null,"Document provider returned an unsupported URI");return;}
        if (request.kind<4 && (data.getFlags()&Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION)!=0) {
            try {getContentResolver().takePersistableUriPermission(uri,Intent.FLAG_GRANT_READ_URI_PERMISSION);}
            catch (SecurityException ignored) { /* Immediate private copy still works for transient providers. */ }
        }
        if (!request.copying.compareAndSet(false,true)) return;
        // Retain Application resources, never the Activity, while a provider
        // stream is open. No foreground/background service is promised here.
        final ContentResolver resolver=getApplicationContext().getContentResolver();
        final File cache=getCacheDir();
        COPIES.execute(() -> copyResult(request,resolver,cache,uri));
    }
    private static void copyResult(Request request, ContentResolver resolver, File cache, Uri uri) {
        File staged=null;
        File stagingDirectory=null;
        try {
            request.check();
            if (request.kind==4 || request.kind==5) {
                File original=validateExport(request);
                try (InputStream input=new FileInputStream(original);
                     OutputStream output=resolver.openOutputStream(uri,"wt")) {
                    if (output==null) throw new IOException("Unable to open export destination");
                    transfer(input,output,PAGE_LIMIT,request);
                    output.flush();
                }
                request.check(); finish(request,original.getAbsolutePath(),request.suggestedName,null);return;
            }
            File directory=new File(cache,"reader-documents");
            if (!directory.isDirectory() && !directory.mkdirs()) throw new IOException("Unable to create import cache");
            if (request.kind==2) {
                List<ImageDocument> images=new ArrayList<>();
                String root=DocumentsContract.getTreeDocumentId(uri);
                String folderName="Image folder";
                try(Cursor cursor=resolver.query(DocumentsContract.buildDocumentUriUsingTree(uri,root),new String[]{DocumentsContract.Document.COLUMN_DISPLAY_NAME},null,null,null)) {
                    if(cursor!=null && cursor.moveToFirst())folderName=displayName(cursor.getString(0));
                } catch(RuntimeException ignored) { /* Names are optional; stream/list errors remain visible. */ }
                if(folderName.trim().isEmpty() || ".".equals(folderName) || "..".equals(folderName))folderName="Image folder";
                stagingDirectory=File.createTempFile("images-","",directory);
                if(!stagingDirectory.delete() || !stagingDirectory.mkdir())throw new IOException("Unable to create image staging directory");
                staged=new File(stagingDirectory,folderName);
                if(!staged.mkdir())throw new IOException("Unable to create image staging directory");
                collect(resolver,uri,root,"",0,new HashSet<>(),new int[]{0},images,request);
                if (images.isEmpty()) throw new IOException("Selected folder has no supported PNG, JPEG, or WebP images");
                images.sort((a,b)->naturalCompare(a.name,b.name));
                long remaining=COMIC_LIMIT;
                for (int i=0;i<images.size();i++) {
                    request.check();ImageDocument image=images.get(i);
                    String suffix=extension(image.name);
                    File target=new File(staged,String.format(Locale.ROOT,"%06d.%s",i,suffix));
                    remaining-=copyUri(resolver,image.uri,target,Math.min(PAGE_LIMIT,remaining),image.size,request);
                }
                request.check();finish(request,staged.getAbsolutePath(),folderName,null);return;
            }
            String suffix=request.kind==3?".json":request.kind==0?".zip":".cbz";
            long cap=request.kind==1?COMIC_LIMIT:PAGE_LIMIT;
            String name="Imported document";long size=-1;
            try (Cursor cursor=resolver.query(uri,new String[]{OpenableColumns.DISPLAY_NAME,OpenableColumns.SIZE},null,null,null)) {
                if (cursor!=null && cursor.moveToFirst()) {name=displayName(cursor.getString(0));if(!cursor.isNull(1))size=cursor.getLong(1);}
            } catch(RuntimeException ignored) { /* Unknown sizes still receive the streaming cap. */ }
            if(name.trim().isEmpty() || ".".equals(name) || "..".equals(name))name="Imported document";
            if(!name.toLowerCase(Locale.ROOT).endsWith(suffix) && !(request.kind==1 && name.toLowerCase(Locale.ROOT).endsWith(".zip")))name+=suffix;
            stagingDirectory=File.createTempFile("document-","",directory);
            if(!stagingDirectory.delete() || !stagingDirectory.mkdir())throw new IOException("Unable to create document staging directory");
            staged=new File(stagingDirectory,name);
            copyUri(resolver,uri,staged,cap,size,request);
            request.check();finish(request,staged.getAbsolutePath(),name,null);
        } catch (Exception error) {
            if (staged!=null) removeStaged(staged);
            if (stagingDirectory!=null) stagingDirectory.delete();
            finish(request,null,null,message(error));
        }
    }
    private static File validateExport(Request request) throws IOException {
        File cache=request.cache.getCanonicalFile();
        File original=new File(request.exportPath).getCanonicalFile();
        if (!original.getPath().startsWith(cache.getPath()+File.separator) || !original.isFile() || original.length()>PAGE_LIMIT) {
            throw new IOException(request.kind==4?"Backup must be a JSON staging file inside the app cache (maximum 32 MiB)":"Image must be a staging file inside the app cache (maximum 32 MiB)");
        }
        return original;
    }
    private static String imageName(String suggestedName,String mime) throws IOException {
        String name=suggestedName==null?"Reader image":suggestedName.replaceAll("[\\p{Cntrl}/\\\\]","_");
        if (name.trim().isEmpty() || ".".equals(name) || "..".equals(name)) name="Reader image";
        String suffix="."+ReaderImageProvider.extensionForMime(mime);
        name=name.toLowerCase(Locale.ROOT).endsWith(suffix)?name:name+suffix;
        if (name.getBytes(StandardCharsets.UTF_8).length>160) throw new IOException("Image filename exceeds 160 UTF-8 bytes");
        return name;
    }
    private static void validateSeriesFolder(String folder) throws IOException {
        if (folder==null || folder.isEmpty()) return;
        if (folder.getBytes(StandardCharsets.UTF_8).length>96 || ".".equals(folder) || "..".equals(folder)
            || !folder.equals(folder.trim()) || folder.endsWith(".") || folder.matches("(?s).*[\\p{Cntrl}/\\\\<>:\"|?*].*")) {
            throw new IOException("Invalid series image folder");
        }
    }
    private static final class ImageTarget {
        final Uri uri;final String name,mime;final long size;final int pending;
        ImageTarget(Uri uri,String name,String mime,long size,int pending) {this.uri=uri;this.name=name;this.mime=mime;this.size=size;this.pending=pending;}
    }
    /** Only app-owned MediaStore rows or documents inside an explicit tree grant. */
    private static final class ImageStore {
        final Request request;final ContentResolver resolver;final boolean tree;final Uri parent;final String relative;
        ImageStore(Request request) throws IOException {
            this.request=request;resolver=request.application.getContentResolver();tree=request.kind==8;
            relative=Environment.DIRECTORY_PICTURES+"/Nova Reader/"+(request.seriesFolder==null || request.seriesFolder.isEmpty()?"":request.seriesFolder+"/");
            if (tree) {
                Uri folder=DocumentsContract.buildDocumentUriUsingTree(request.tree,DocumentsContract.getTreeDocumentId(request.tree));
                folder=directory(folder,"Nova Reader");
                if (request.seriesFolder!=null && !request.seriesFolder.isEmpty()) folder=directory(folder,request.seriesFolder);
                parent=folder;
            } else {parent=MediaStore.Images.Media.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY);}
        }
        private Uri directory(Uri parent,String name) throws IOException {
            ImageTarget found=findTree(parent,name);
            if (found!=null) {
                if (!DocumentsContract.Document.MIME_TYPE_DIR.equals(found.mime)) throw new IOException("Image destination contains a file named "+name);
                return found.uri;
            }
            Uri created=DocumentsContract.createDocument(resolver,parent,DocumentsContract.Document.MIME_TYPE_DIR,name);
            if (created==null) throw new IOException("Unable to create image destination folder");return created;
        }
        private ImageTarget findTree(Uri folder,String name) throws IOException {
            Uri children=DocumentsContract.buildChildDocumentsUriUsingTree(request.tree,DocumentsContract.getDocumentId(folder));
            try (Cursor cursor=resolver.query(children,new String[]{DocumentsContract.Document.COLUMN_DOCUMENT_ID,DocumentsContract.Document.COLUMN_DISPLAY_NAME,DocumentsContract.Document.COLUMN_MIME_TYPE,DocumentsContract.Document.COLUMN_SIZE},null,null,null)) {
                if (cursor==null) throw new IOException("Unable to inspect image destination");
                int count=0;while (cursor.moveToNext()) {
                    request.check();if (++count>ENTRY_LIMIT) throw new IOException("Image destination contains too many documents");
                    if (name.equals(cursor.getString(1))) return new ImageTarget(DocumentsContract.buildDocumentUriUsingTree(request.tree,cursor.getString(0)),cursor.getString(1),cursor.getString(2),cursor.isNull(3)?-1:cursor.getLong(3),0);
                }
            }
            return null;
        }
        private ImageTarget media(Uri uri,String name) throws IOException {
            String selection=MediaStore.Images.Media.RELATIVE_PATH+"=? AND "+MediaStore.Images.Media.OWNER_PACKAGE_NAME+"=?";
            String[] arguments;
            if (name==null) arguments=new String[]{relative,request.application.getPackageName()};
            else {selection+=" AND "+MediaStore.Images.Media.DISPLAY_NAME+"=?";arguments=new String[]{relative,request.application.getPackageName(),name};}
            try (Cursor cursor=resolver.query(uri,new String[]{MediaStore.Images.Media._ID,MediaStore.Images.Media.DISPLAY_NAME,MediaStore.Images.Media.MIME_TYPE,MediaStore.Images.Media.SIZE,MediaStore.Images.Media.IS_PENDING},selection,arguments,MediaStore.Images.Media._ID+" DESC")) {
                if (cursor==null) throw new IOException("Unable to inspect saved images");
                if (!cursor.moveToFirst()) return null;
                return new ImageTarget(ContentUris.withAppendedId(parent,cursor.getLong(0)),cursor.getString(1),cursor.getString(2),cursor.isNull(3)?-1:cursor.getLong(3),(int)cursor.getLong(4));
            }
        }
        ImageTarget find(String name) throws IOException {return tree?findTree(parent,name):media(parent,name);}
        ImageTarget stat(Uri uri) throws IOException {
            if (!tree) return media(uri,null);
            try (Cursor cursor=resolver.query(uri,new String[]{DocumentsContract.Document.COLUMN_DOCUMENT_ID,DocumentsContract.Document.COLUMN_DISPLAY_NAME,DocumentsContract.Document.COLUMN_MIME_TYPE,DocumentsContract.Document.COLUMN_SIZE},null,null,null)) {
                if (cursor==null || !cursor.moveToFirst()) return null;
                return new ImageTarget(uri,cursor.getString(1),cursor.getString(2),cursor.isNull(3)?-1:cursor.getLong(3),0);
            }
        }
        Uri create(String name) throws IOException {
            if (tree) return DocumentsContract.createDocument(resolver,parent,request.mime,".reader-"+UUID.randomUUID()+"."+ReaderImageProvider.extensionForMime(request.mime));
            ContentValues metadata=new ContentValues();
            metadata.put(MediaStore.Images.Media.DISPLAY_NAME,name);metadata.put(MediaStore.Images.Media.MIME_TYPE,request.mime);
            metadata.put(MediaStore.Images.Media.RELATIVE_PATH,relative);metadata.put(MediaStore.Images.Media.IS_PENDING,1);
            return resolver.insert(parent,metadata);
        }
        void pending(Uri uri,int value,String mime) throws IOException {
            if (tree) return;
            ContentValues metadata=new ContentValues();metadata.put(MediaStore.Images.Media.IS_PENDING,value);metadata.put(MediaStore.Images.Media.MIME_TYPE,mime);
            if (resolver.update(uri,metadata,null,null)!=1) throw new IOException("Unable to publish saved image");
        }
        Uri publish(Uri uri,String name) throws IOException {
            if (!tree) {pending(uri,0,request.mime);return uri;}
            if (find(name)!=null) throw new IOException("Image destination changed while saving; try again");
            Uri renamed=DocumentsContract.renameDocument(resolver,uri,name);
            if (renamed==null) throw new IOException("Unable to publish saved image");
            ImageTarget actual=stat(renamed);
            if (actual==null || !name.equals(actual.name)) {remove(renamed);throw new IOException("Image provider changed the requested filename");}
            return renamed;
        }
        void remove(Uri uri) throws IOException {
            if (tree) {if (!DocumentsContract.deleteDocument(resolver,uri)) throw new IOException("Unable to remove incomplete image");}
            else resolver.delete(uri,null,null);
        }
    }
    private static void prepareImageSave(Request request) {
        try {
            request.check();request.store=new ImageStore(request);
            request.target=request.store.find(request.targetName);
            if (request.target==null) {writeNewImage(request);return;}
            if (request.collisionPolicy==2) {finish(request,null,null,"Image already exists; nothing saved.");return;}
            if (request.collisionPolicy==1) {overwriteImage(request);return;}
            synchronized (LOCK) {
                request.check();if (active!=request) return;request.awaitingChoice=true;
            }
            nativeImageConflict(request.id,request.targetName);
        } catch (Exception error) {imageFailure(request,error);}
    }
    public static void resolveImage(long id,int choice) {
        Request request;
        synchronized (LOCK) {
            request=active;if (request==null || request.id!=id || !request.awaitingChoice) return;
            request.awaitingChoice=false;
        }
        if (choice==2) {finish(request,null,null,"Image action cancelled");return;}
        COPIES.execute(() -> {
            try {
                request.check();
                if (choice==0) overwriteImage(request);
                else if (choice==1) {
                    String original=imageName(request.suggestedName,request.mime);
                    int dot=original.lastIndexOf('.');String extension=original.substring(dot),stem=original.substring(0,dot);
                    boolean selected=false;
                    for (int number=2;number<=ENTRY_LIMIT;number++) {
                        request.check();String suffix=" ("+number+")"+extension;String base=stem;
                        while ((base+suffix).getBytes(StandardCharsets.UTF_8).length>160) base=base.substring(0,base.offsetByCodePoints(base.length(),-1));
                        String candidate=base+suffix;
                        if (request.store.find(candidate)==null) {request.targetName=candidate;selected=true;break;}
                    }
                    if (!selected) throw new IOException("No unused image filename is available");
                    writeNewImage(request);
                } else throw new IOException("Unknown image collision choice");
            } catch (Exception error) {imageFailure(request,error);}
        });
    }
    private static void writeNewImage(Request request) throws IOException {
        Uri destination=null;boolean published=false;
        try {
            request.check();File original=validateExport(request);destination=request.store.create(request.targetName);
            if (destination==null) throw new IOException("Unable to create saved image");
            if (!request.store.tree) {
                ImageTarget actual=request.store.stat(destination);
                if (actual==null || !request.targetName.equals(actual.name)) throw new IOException("Image provider changed the requested filename");
            }
            copyImageBytes(request,request.store.resolver,original,destination,true);
            synchronized (LOCK) {
                request.check();if (active!=request) throw new IOException("Image action cancelled");
                destination=request.store.publish(destination,request.targetName);published=true;
                finish(request,original.getAbsolutePath(),request.targetName,null);
            }
        } finally {if (destination!=null && !published) request.store.remove(destination);}
    }
    private static void overwriteImage(Request request) throws IOException {
        request.check();ImageTarget current=request.store.find(request.targetName);
        if (current==null || request.target==null || !current.uri.toString().equals(request.target.uri.toString())) throw new IOException("Existing image changed; save again to choose its replacement");
        if (current.size>PAGE_LIMIT || DocumentsContract.Document.MIME_TYPE_DIR.equals(current.mime)) throw new IOException("Existing image cannot be safely replaced");
        if (request.store.tree && !request.mime.equals(current.mime)) throw new IOException("Existing document has a different image type; choose Add number");
        File[] recovery=request.cache.listFiles(file -> file.getName().startsWith("reader-image-old-") && file.getName().endsWith(".bak"));
        if (recovery==null || recovery.length>0) throw new IOException("A previous image recovery is retained in the app cache; automatic overwrite is unavailable");
        File backup=File.createTempFile("reader-image-old-",".bak",request.cache);
        boolean keepBackup=false,touched=false;
        try {
            try (InputStream input=request.store.resolver.openInputStream(current.uri);FileOutputStream output=new FileOutputStream(backup)) {
                if (input==null) throw new IOException("Unable to back up existing image");
                transfer(input,output,PAGE_LIMIT,request);output.getFD().sync();
            }
            request.check();File original=validateExport(request);
            touched=true;request.store.pending(current.uri,1,current.mime);
            copyImageBytes(request,request.store.resolver,original,current.uri,true);
            synchronized (LOCK) {
                request.check();if (active!=request) throw new IOException("Image action cancelled");
                request.store.pending(current.uri,0,request.mime);
                finish(request,original.getAbsolutePath(),request.targetName,null);
            }
        } catch (Exception error) {
            String failure=message(error);
            if (touched) {
                try {copyImageBytes(request,request.store.resolver,backup,current.uri,false);}
                catch (Exception restore) {keepBackup=true;failure+="; old image backup retained after recovery failed: "+message(restore);}
                try {request.store.pending(current.uri,current.pending,current.mime);}
                catch (Exception restore) {failure+="; old image visibility recovery failed: "+message(restore);}
            }
            throw new IOException(failure,error);
        } finally {if (!keepBackup) backup.delete();}
    }
    private static void copyImageBytes(Request request,ContentResolver resolver,File source,Uri destination,boolean cancellable) throws IOException {
        try (InputStream input=new FileInputStream(source);OutputStream output=resolver.openOutputStream(destination,"wt")) {
            if (output==null) throw new IOException("Unable to open saved image");
            if (cancellable) transfer(input,output,PAGE_LIMIT,request);
            else {byte[] buffer=new byte[65536];long total=0;int count;while ((count=input.read(buffer))!=-1) {if (count>PAGE_LIMIT-total) throw new IOException("Image recovery exceeds size limit");output.write(buffer,0,count);total+=count;}}
            output.flush();
        }
    }
    private static void imageFailure(Request request,Exception error) {
        if (error instanceof SecurityException && request.kind==8) request.application.getSharedPreferences("reader-image-export",Context.MODE_PRIVATE).edit().remove("tree").apply();
        finish(request,null,null,message(error));
    }
    private static void copyToClipboard(Request request,Context application,File original) {
        File image=null;
        try {
            request.check();
            image=ReaderImageProvider.stage(application,original,request.mime,request.cancelled);
            final File retained=image;
            boolean posted=new Handler(Looper.getMainLooper()).post(() -> {
                try {
                    ReaderActivity activity=foreground.get();
                    if (activity==null || activity.isFinishing() || activity.isDestroyed()) throw new IOException("Reader document activity is unavailable");
                    synchronized (LOCK) {
                        request.check();
                        if (active!=request) throw new IOException("Document selection cancelled");
                        ClipboardManager clipboard=(ClipboardManager)application.getSystemService(Context.CLIPBOARD_SERVICE);
                        if (clipboard==null) throw new IOException("Android clipboard is unavailable");
                        clipboard.setPrimaryClip(ClipData.newUri(application.getContentResolver(),"Reader image",ReaderImageProvider.uriFor(retained)));
                    }
                    finish(request,original.getAbsolutePath(),request.suggestedName,null);
                } catch (Exception error) {retained.delete();finish(request,null,null,message(error));}
            });
            if (!posted) throw new IOException("Reader UI thread is unavailable");
        } catch (Exception error) {
            if (image!=null) image.delete();
            finish(request,null,null,message(error));
        }
    }
    private static long copyUri(ContentResolver resolver,Uri uri,File target,long cap,long declared,Request request) throws IOException {
        if (declared>cap) throw new IOException("Selected document exceeds the import size limit");
        try (InputStream input=resolver.openInputStream(uri);FileOutputStream output=new FileOutputStream(target)) {
            if (input==null) throw new IOException("Unable to open selected document");
            long total=transfer(input,output,cap,request);output.getFD().sync();return total;
        }
    }
    private static long transfer(InputStream input,OutputStream output,long cap,Request request) throws IOException {
        byte[] buffer=new byte[65536];long total=0;int emptyReads=0;
        while (true) {request.check();int count=input.read(buffer);if(count<0)break;if(count==0){if(++emptyReads>16)throw new IOException("Document provider stopped making progress");continue;}emptyReads=0;
            if (count>cap-total) throw new IOException("Selected document exceeds the import size limit");
            output.write(buffer,0,count);total+=count;
        }
        return total;
    }
    private static void collect(ContentResolver resolver,Uri tree,String id,String prefix,int depth,Set<String> seen,int[] entries,List<ImageDocument> images,Request request) throws IOException {
        request.check();
        if (depth>8 || !seen.add(id)) throw new IOException("Folder is too deep or contains a document cycle");
        Uri children=DocumentsContract.buildChildDocumentsUriUsingTree(tree,id);
        String[] projection={DocumentsContract.Document.COLUMN_DOCUMENT_ID,DocumentsContract.Document.COLUMN_DISPLAY_NAME,DocumentsContract.Document.COLUMN_MIME_TYPE,DocumentsContract.Document.COLUMN_SIZE};
        try (Cursor cursor=resolver.query(children,projection,null,null,null)) {
            if (cursor==null) throw new IOException("Unable to list selected folder");
            while (cursor.moveToNext()) {
                request.check();if(++entries[0]>ENTRY_LIMIT)throw new IOException("Folder contains too many entries");
                String child=cursor.getString(0);String name=displayName(cursor.getString(1));String mime=cursor.getString(2);
                if (DocumentsContract.Document.MIME_TYPE_DIR.equals(mime)) collect(resolver,tree,child,prefix+name+"/",depth+1,seen,entries,images,request);
                else if (supported(name)) images.add(new ImageDocument(DocumentsContract.buildDocumentUriUsingTree(tree,child),prefix+name,cursor.isNull(3)?-1:cursor.getLong(3)));
            }
        }
    }
    private static String displayName(String name) {return name==null?"Document":name.substring(0,Math.min(128,name.length())).replaceAll("[\\p{Cntrl}/\\\\]","_");}
    private static String extension(String name) {int dot=name.lastIndexOf('.');return dot<0?"":name.substring(dot+1).toLowerCase(Locale.ROOT);}
    private static boolean supported(String name) {String ext=extension(name);return "png".equals(ext)||"jpg".equals(ext)||"jpeg".equals(ext)||"webp".equals(ext)||"avif".equals(ext)||"jxl".equals(ext);}
    private static int naturalCompare(String left,String right) {
        int a=0,b=0;
        while(a<left.length() && b<right.length()) {
            char x=left.charAt(a),y=right.charAt(b);
            if(x>='0' && x<='9' && y>='0' && y<='9') {
                int startA=a,startB=b;while(a<left.length() && left.charAt(a)>='0' && left.charAt(a)<='9')a++;while(b<right.length() && right.charAt(b)>='0' && right.charAt(b)<='9')b++;
                while(startA<a-1 && left.charAt(startA)=='0')startA++;while(startB<b-1 && right.charAt(startB)=='0')startB++;
                int order=Integer.compare(a-startA,b-startB);if(order==0)order=left.substring(startA,a).compareTo(right.substring(startB,b));if(order!=0)return order;
            } else {int order=Character.compare(Character.toLowerCase(x),Character.toLowerCase(y));if(order!=0)return order;a++;b++;}
        }
        int order=Integer.compare(left.length()-a,right.length()-b);return order!=0?order:left.compareTo(right);
    }
    private static void removeStaged(File file) {if(file.isDirectory()){File[] files=file.listFiles();if(files!=null)for(File child:files)child.delete();}file.delete();}
    private static String message(Exception error) {String message=error.getMessage();return message==null?error.getClass().getSimpleName():message;}
    private static void finish(Request request,String path,String name,String error) {
        synchronized (LOCK) {if(active!=request)return;active=null;}
        nativeDocumentResult(request.id,path,name,error);
    }
    @Override protected void onCreate(Bundle state) {
        foreground=new WeakReference<>(this);
        super.onCreate(state);
        normalCutoutMode=Build.VERSION.SDK_INT>=30
            ?WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS
            :WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
        applyReaderImmersive();
        synchronized(LOCK) {if(active!=null)active.activity=new WeakReference<>(this);}
    }
    @Override protected void onResume() {
        super.onResume();
        notificationForeground=new WeakReference<>(this);
        activityResumed=true;
        if (normalWakePending && !readerPresentation.reading) {
            if (normalKeepAwake) getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
            else getWindow().clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        }
        applyReaderImmersive();
        normalWakePending=false;
    }
    @Override protected void onPause() {
        if(notificationForeground.get()==this)notificationForeground=new WeakReference<>(null);
        if (!readerWindowActive) normalKeepAwake=(getWindow().getAttributes().flags&WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)!=0;
        normalWakePending=true;
        activityResumed=false;
        getWindow().clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        super.onPause();
    }
    @Override protected void onDestroy() {
        if(notificationForeground.get()==this)notificationForeground=new WeakReference<>(null);
        activityResumed=false;
        getWindow().clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        if(foreground.get()==this)foreground=new WeakReference<>(null);
        Request request; synchronized(LOCK){request=active;}
        if(request!=null && request.activity.get()==this && !isChangingConfigurations())cancelDocument(request.id);
        super.onDestroy();
    }
}
