package dev.misob.nova.reader;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.content.Context;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.OpenableColumns;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileNotFoundException;
import java.io.FileOutputStream;
import java.io.IOException;
import java.util.Arrays;
import java.util.List;
import java.util.UUID;
import java.util.concurrent.atomic.AtomicBoolean;

/** Read-only clipboard images. Android enforces per-URI grants on this private provider. */
public final class ReaderImageProvider extends ContentProvider {
    private static final String AUTHORITY="dev.misob.nova.reader.images";
    private static final long LIMIT=32L*1024*1024;
    private static final long RETENTION=24L*60*60*1000;
    private static final int MAX_FILES=8;
    private static final String NAME="[0-9a-f-]{36}\\.(png|jpg|webp|gif)";

    static String extensionForMime(String mime) throws IOException {
        if ("image/png".equals(mime)) return "png";
        if ("image/jpeg".equals(mime)) return "jpg";
        if ("image/webp".equals(mime)) return "webp";
        if ("image/gif".equals(mime)) return "gif";
        throw new IOException("Unsupported clipboard/export image type");
    }
    private static File directory(Context context) throws IOException {
        File cache=context.getCacheDir().getCanonicalFile();
        File directory=new File(cache,"reader-clipboard");
        if (!directory.isDirectory() && !directory.mkdir()) throw new IOException("Unable to create image clipboard cache");
        if (!directory.getCanonicalFile().getParentFile().equals(cache)) throw new IOException("Invalid image clipboard cache");
        return directory;
    }
    static synchronized File stage(Context context,File source,String mime,AtomicBoolean cancelled) throws IOException {
        String extension=extensionForMime(mime);
        File directory=directory(context);
        File[] images=directory.listFiles(file -> file.getName().matches(NAME));
        if (images==null) throw new IOException("Unable to list image clipboard cache");
        Arrays.sort(images,(left,right)->Long.compare(right.lastModified(),left.lastModified()));
        long now=System.currentTimeMillis();
        for (int i=0;i<images.length;i++) {
            if ((i>=MAX_FILES-1 || now-images[i].lastModified()>RETENTION) && !images[i].delete()) {
                throw new IOException("Unable to retire old clipboard image");
            }
        }
        File image=new File(directory,UUID.randomUUID().toString()+"."+extension);
        boolean complete=false;
        try {
            try (FileInputStream input=new FileInputStream(source);FileOutputStream output=new FileOutputStream(image)) {
                byte[] buffer=new byte[65536];long total=0;
                while (true) {
                    if (cancelled.get()) throw new IOException("Document selection cancelled");
                    int count=input.read(buffer);if (count<0) break;
                    if (count>LIMIT-total) throw new IOException("Image exceeds the 32 MiB clipboard limit");
                    output.write(buffer,0,count);total+=count;
                }
                output.getFD().sync();
            }
            if (cancelled.get()) throw new IOException("Document selection cancelled");
            complete=true;return image;
        } finally {if (!complete) image.delete();}
    }
    static Uri uriFor(File image) {
        return new Uri.Builder().scheme("content").authority(AUTHORITY).appendPath(image.getName()).build();
    }
    private File resolve(Uri uri) throws FileNotFoundException {
        try {
            List<String> segments=uri.getPathSegments();
            if (!"content".equals(uri.getScheme()) || !AUTHORITY.equals(uri.getAuthority()) || segments.size()!=1 || !segments.get(0).matches(NAME)) {
                throw new IOException("Invalid clipboard image URI");
            }
            File directory=directory(getContext());
            File image=new File(directory,segments.get(0)).getCanonicalFile();
            if (!directory.getCanonicalFile().equals(image.getParentFile()) || !image.isFile() || image.length()>LIMIT || System.currentTimeMillis()-image.lastModified()>RETENTION) {
                throw new IOException("Clipboard image is no longer available");
            }
            return image;
        } catch (IOException error) {throw new FileNotFoundException(error.getMessage());}
    }
    @Override public boolean onCreate() { return true; }
    @Override public String getType(Uri uri) {
        try {
            String name=resolve(uri).getName();
            if (name.endsWith(".png")) return "image/png";
            if (name.endsWith(".jpg")) return "image/jpeg";
            if (name.endsWith(".gif")) return "image/gif";
            return "image/webp";
        } catch (FileNotFoundException error) {return null;}
    }
    @Override public ParcelFileDescriptor openFile(Uri uri,String mode) throws FileNotFoundException {
        if (!"r".equals(mode)) throw new FileNotFoundException("Clipboard images are read-only");
        return ParcelFileDescriptor.open(resolve(uri),ParcelFileDescriptor.MODE_READ_ONLY);
    }
    @Override public Cursor query(Uri uri,String[] projection,String selection,String[] selectionArgs,String sortOrder) {
        try {
            File image=resolve(uri);
            String[] columns=projection==null?new String[]{OpenableColumns.DISPLAY_NAME,OpenableColumns.SIZE}:projection;
            MatrixCursor result=new MatrixCursor(columns,1);
            Object[] values=new Object[columns.length];
            for (int i=0;i<columns.length;i++) {
                if (OpenableColumns.DISPLAY_NAME.equals(columns[i])) values[i]="Reader image"+image.getName().substring(image.getName().lastIndexOf('.'));
                else if (OpenableColumns.SIZE.equals(columns[i])) values[i]=image.length();
            }
            result.addRow(values);return result;
        } catch (FileNotFoundException error) {return null;}
    }
    @Override public Uri insert(Uri uri,ContentValues values) {throw new UnsupportedOperationException("Read-only image provider");}
    @Override public int update(Uri uri,ContentValues values,String selection,String[] args) {throw new UnsupportedOperationException("Read-only image provider");}
    @Override public int delete(Uri uri,String selection,String[] args) {throw new UnsupportedOperationException("Read-only image provider");}
}
