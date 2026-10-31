package android.content;
import android.database.Cursor;
import android.net.Uri;
import android.provider.MediaStore;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
public class ContentResolver {
    public static ContentProvider provider;
    public static ByteArrayOutputStream exported;
    public static boolean failOutput;
    public static boolean failInsert,failWrite,failClose,failCommit;
    public static Runnable duringWrite;
    public static final java.util.Map<String,ContentValues> mediaRows=new java.util.LinkedHashMap<>();
    public static ContentValues lastMedia;
    public static int inserts,deletes,commits;
    public static void resetMedia() {
        mediaRows.clear();lastMedia=null;inserts=deletes=commits=0;
        failOutput=failInsert=failWrite=failClose=failCommit=false;duringWrite=null;
    }
    public Uri insert(Uri collection,ContentValues values) {
        if (failInsert) return null;
        if (!Integer.valueOf(1).equals(values.getAsInteger(MediaStore.Images.Media.IS_PENDING))) throw new AssertionError("Image must start pending");
        Uri uri=Uri.parse(collection.toString()+"/"+(++inserts));
        lastMedia=new ContentValues(values);mediaRows.put(uri.toString(),new ContentValues(values));return uri;
    }
    public int update(Uri uri,ContentValues values,String selection,String[] args) {
        if (failCommit) throw new IllegalStateException("fixture publish failure");
        ContentValues row=mediaRows.get(uri.toString());if (row==null) return 0;
        row.putAll(values);commits++;return 1;
    }
    public int delete(Uri uri,String selection,String[] args) { if (mediaRows.remove(uri.toString())!=null) {deletes++;return 1;}return 0; }
    public void takePersistableUriPermission(Uri uri,int flags) { }
    public Cursor query(Uri uri,String[] projection,String selection,String[] args,String sort) { return provider.query(uri,projection,selection,args,sort); }
    public String getType(Uri uri) { return provider==null?null:provider.getType(uri); }
    public InputStream openInputStream(Uri uri) throws IOException { throw new IOException("No input fixture"); }
    public OutputStream openOutputStream(Uri uri,String mode) throws IOException {
        if (failOutput) throw new IOException("fixture destination failure");
        ContentValues row=mediaRows.get(uri.toString());
        if (row!=null && !Integer.valueOf(1).equals(row.getAsInteger(MediaStore.Images.Media.IS_PENDING))) throw new AssertionError("Image exposed before copy");
        exported=new ByteArrayOutputStream();
        final ByteArrayOutputStream buffer=exported;
        return new OutputStream() {
            public void write(int value) throws IOException {write(new byte[]{(byte)value},0,1);}
            public void write(byte[] bytes,int start,int count) throws IOException {
                if (duringWrite!=null) {Runnable callback=duringWrite;duringWrite=null;callback.run();}
                if (failWrite) throw new IOException("fixture image write failure");
                buffer.write(bytes,start,count);
            }
            public void close() throws IOException {if (failClose) throw new IOException("fixture image close failure");}
        };
    }
}
