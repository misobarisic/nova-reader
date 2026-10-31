package dev.misob.nova.reader;

import android.app.NativeActivity;
import android.content.ContentResolver;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.Intent;
import android.database.Cursor;
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
import android.view.View;
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
    private static volatile boolean readerImmersive;
    private static native void nativeDocumentResult(long id, String path, String name, String error);

    private static final class Request {
        final long id;
        final int kind;
        final String exportPath;
        final String suggestedName;
        final String mime;
        final String seriesFolder;
        final AtomicBoolean cancelled = new AtomicBoolean();
        final AtomicBoolean copying = new AtomicBoolean();
        volatile WeakReference<ReaderActivity> activity;
        final File cache;
        final int requestCode;
        Request(long id, int kind, String exportPath, String suggestedName, String mime, String seriesFolder, ReaderActivity activity) {
            this.id=id; this.kind=kind; this.exportPath=exportPath;
            this.suggestedName=suggestedName; this.mime=mime;
            this.seriesFolder=seriesFolder;
            this.activity=new WeakReference<>(activity);
            this.cache=activity.getCacheDir();
            this.requestCode=(int)id;
        }
        void check() throws IOException {
            if (cancelled.get()) throw new IOException("Document selection cancelled");
        }
    }
    private static final class ImageDocument {
        final Uri uri; final String name; final long size;
        ImageDocument(Uri uri, String name, long size) {this.uri=uri;this.name=name;this.size=size;}
    }

    public static void setReaderImmersive(boolean immersive) {
        // Keep the latest intent even between activities. Resolve the weak
        // reference on main so a queued update never touches a destroyed window.
        readerImmersive=immersive;
        boolean posted=new Handler(Looper.getMainLooper()).post(() -> {
            ReaderActivity activity=foreground.get();
            if (activity!=null && !activity.isFinishing() && !activity.isDestroyed()) {
                activity.applyReaderImmersive();
            }
        });
        if (!posted) throw new IllegalStateException("Reader UI thread is unavailable");
    }
    @SuppressWarnings("deprecation")
    private void applyReaderImmersive() {
        if (Build.VERSION.SDK_INT>=30) {
            WindowInsetsController controller=getWindow().getInsetsController();
            if (controller!=null) {
                controller.setSystemBarsBehavior(WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);
                if (readerImmersive) controller.hide(WindowInsets.Type.statusBars());
                else controller.show(WindowInsets.Type.statusBars());
            }
        } else {
            View decor=getWindow().getDecorView();
            int flags=decor.getSystemUiVisibility();
            int readerFlags=View.SYSTEM_UI_FLAG_FULLSCREEN|View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY;
            decor.setSystemUiVisibility(readerImmersive?flags|readerFlags:flags&~readerFlags);
        }
    }
    @Override public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (hasFocus) applyReaderImmersive();
    }

    public static void requestDocument(int kind, long id, String exportPath) {
        queueDocument(kind,id,exportPath,"nova-reader-backup.json","application/json","");
    }
    public static void requestImage(int kind, long id, String exportPath, String suggestedName, String mime) {
        if (kind!=5 && kind!=6) {nativeDocumentResult(id,null,null,"Unknown image operation");return;}
        queueDocument(kind,id,exportPath,suggestedName,mime,"");
    }
    public static void publishImage(long id, String exportPath, String suggestedName, String mime, String seriesFolder) {
        queueDocument(Build.VERSION.SDK_INT>=29?7:5,id,exportPath,suggestedName,mime,seriesFolder);
    }
    private static void queueDocument(int kind, long id, String exportPath, String suggestedName, String mime, String seriesFolder) {
        // ndk-context exposes the Application, not an Activity. Resolve the
        // current Reader activity after posting, so recreation cannot leave a
        // queued picker launch targeting the activity that was destroyed.
        boolean posted=new Handler(Looper.getMainLooper()).post(() -> {
            ReaderActivity activity=foreground.get();
            if (activity==null || activity.isFinishing() || activity.isDestroyed()) {
                nativeDocumentResult(id,null,null,"Reader document activity is unavailable"); return;
            }
            activity.openDocument(kind,id,exportPath,suggestedName,mime,seriesFolder);
        });
        if (!posted) nativeDocumentResult(id,null,null,"Reader UI thread is unavailable");
    }
    private void openDocument(int kind, long id, String exportPath, String suggestedName, String mime, String seriesFolder) {
        if (isFinishing() || isDestroyed()) {
            nativeDocumentResult(id,null,null,"Reader activity is closing"); return;
        }
        if(id<1 || id>65535) {nativeDocumentResult(id,null,null,"Restart the app to open another document");return;}
        Request request=new Request(id,kind,exportPath,suggestedName,mime,seriesFolder,this);
        synchronized (LOCK) {
            if(id<=lastCancelledId) {nativeDocumentResult(id,null,null,"Document selection cancelled");return;}
            if (active!=null) {nativeDocumentResult(id,null,null,"Another document request is active");return;}
            active=request;
        }
        try {
            if (kind==7) {
                File original=validateExport(request);
                ReaderImageProvider.extensionForMime(mime);
                validateSeriesFolder(seriesFolder);
                request.copying.set(true);
                ContentResolver resolver=getApplicationContext().getContentResolver();
                COPIES.execute(() -> publishToMediaStore(request,resolver,original));
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
        String name=displayName(suggestedName);
        if (name.trim().isEmpty() || ".".equals(name) || "..".equals(name)) name="Reader image";
        String suffix="."+ReaderImageProvider.extensionForMime(mime);
        return name.toLowerCase(Locale.ROOT).endsWith(suffix)?name:name+suffix;
    }
    private static void validateSeriesFolder(String folder) throws IOException {
        if (folder==null || folder.isEmpty()) return;
        if (folder.getBytes(StandardCharsets.UTF_8).length>80 || ".".equals(folder) || "..".equals(folder)
            || !folder.equals(folder.trim()) || folder.endsWith(".") || folder.matches("(?s).*[\\p{Cntrl}/\\\\<>:\"|?*].*")) {
            throw new IOException("Invalid series image folder");
        }
    }
    private static void publishToMediaStore(Request request,ContentResolver resolver,File original) {
        Uri destination=null;
        boolean published=false;
        try {
            request.check();
            ContentValues metadata=new ContentValues();
            metadata.put(MediaStore.Images.Media.DISPLAY_NAME,imageName(request.suggestedName,request.mime));
            metadata.put(MediaStore.Images.Media.MIME_TYPE,request.mime);
            String folder=Environment.DIRECTORY_PICTURES+"/Nova Reader/";
            if (request.seriesFolder!=null && !request.seriesFolder.isEmpty()) folder+=request.seriesFolder+"/";
            metadata.put(MediaStore.Images.Media.RELATIVE_PATH,folder);
            metadata.put(MediaStore.Images.Media.IS_PENDING,1);
            // A fresh row gives MediaStore responsibility for filename collisions.
            destination=resolver.insert(MediaStore.Images.Media.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY),metadata);
            if (destination==null) throw new IOException("Unable to create saved image");
            try (InputStream input=new FileInputStream(original);OutputStream output=resolver.openOutputStream(destination,"w")) {
                if (output==null) throw new IOException("Unable to open saved image");
                transfer(input,output,PAGE_LIMIT,request);output.flush();
            }
            synchronized (LOCK) {
                request.check();
                if (active!=request) throw new IOException("Document selection cancelled");
                ContentValues ready=new ContentValues();
                ready.put(MediaStore.Images.Media.IS_PENDING,0);
                if (resolver.update(destination,ready,null,null)!=1) throw new IOException("Unable to publish saved image");
                published=true;
                finish(request,original.getAbsolutePath(),request.suggestedName,null);
            }
        } catch (Exception error) {
            String failure=message(error);
            if (destination!=null && !published) {
                try {resolver.delete(destination,null,null);}
                catch (Exception cleanup) {failure+="; image cleanup failed: "+message(cleanup);}
            }
            finish(request,null,null,failure);
        }
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
    private static boolean supported(String name) {String ext=extension(name);return "png".equals(ext)||"jpg".equals(ext)||"jpeg".equals(ext)||"webp".equals(ext);}
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
        applyReaderImmersive();
        synchronized(LOCK) {if(active!=null)active.activity=new WeakReference<>(this);}
    }
    @Override protected void onDestroy() {
        if(foreground.get()==this)foreground=new WeakReference<>(null);
        Request request; synchronized(LOCK){request=active;}
        if(request!=null && request.activity.get()==this && !isChangingConfigurations())cancelDocument(request.id);
        super.onDestroy();
    }
}
