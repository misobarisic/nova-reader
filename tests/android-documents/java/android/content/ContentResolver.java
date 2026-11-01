package android.content;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.net.Uri;
import android.provider.MediaStore;
import android.provider.DocumentsContract;
import java.io.ByteArrayOutputStream;
import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
public class ContentResolver {
    public static ContentProvider provider;
    public static ByteArrayOutputStream exported;
    public static boolean failOutput,failInsert,failWrite,failClose,failCommit;
    public static boolean failWriteOnce,failCloseOnce,failPublishOnce,failQuery,ownershipQueried;
    public static Runnable duringWrite;
    public static final java.util.Map<String,ContentValues> mediaRows=new java.util.LinkedHashMap<>();
    public static final java.util.Map<String,byte[]> data=new java.util.LinkedHashMap<>();
    public static final java.util.Map<String,ContentValues> documents=new java.util.LinkedHashMap<>();
    public static ContentValues lastMedia;
    public static int inserts,deletes,commits,nextDocument;
    public static void resetMedia() {
        mediaRows.clear();data.clear();lastMedia=null;inserts=deletes=commits=0;
        failOutput=failInsert=failWrite=failClose=failCommit=failWriteOnce=failCloseOnce=failPublishOnce=failQuery=ownershipQueried=false;duringWrite=null;
    }
    public static void resetTree() {
        documents.clear();nextDocument=0;
        ContentValues root=new ContentValues();root.put("document_id","root");root.put("_display_name","Pictures");root.put("mime_type",DocumentsContract.Document.MIME_TYPE_DIR);root.put("_size",0L);
        documents.put("root",root);
        new android.app.Application().getSharedPreferences("reader-image-export",0).edit().remove("tree").apply();
    }
    public Uri insert(Uri collection,ContentValues values) {
        if (failInsert) return null;
        if (!Integer.valueOf(1).equals(values.getAsInteger(MediaStore.Images.Media.IS_PENDING))) throw new AssertionError("Image must start pending");
        String requested=values.getAsString("_display_name"),name=requested;int suffix=1;
        while (nameExists(name,values.getAsString("relative_path"))) name=requested+" ("+(suffix++)+")";
        Uri uri=Uri.parse(collection.toString()+"/"+(++inserts));
        ContentValues row=new ContentValues(values);row.put("_display_name",name);row.put("owner_package_name","dev.misob.nova.reader");row.put("_id",(long)inserts);row.put("_size",0L);
        lastMedia=new ContentValues(row);mediaRows.put(uri.toString(),row);data.put(uri.toString(),new byte[0]);return uri;
    }
    private static boolean nameExists(String name,String path) {
        for (ContentValues row:mediaRows.values()) if (name.equals(row.get("_display_name")) && path.equals(row.get("relative_path"))) return true;
        return false;
    }
    public int update(Uri uri,ContentValues values,String selection,String[] args) {
        if (failCommit) throw new IllegalStateException("fixture publish failure");
        if (failPublishOnce && Integer.valueOf(0).equals(values.get("is_pending"))) {failPublishOnce=false;throw new IllegalStateException("fixture publish failure");}
        ContentValues row=mediaRows.get(uri.toString());if (row==null) return 0;
        row.putAll(values);if (Integer.valueOf(0).equals(values.get("is_pending"))) commits++;return 1;
    }
    public int delete(Uri uri,String selection,String[] args) {
        ContentValues removed=mediaRows.remove(uri.toString());
        if (removed==null && "fixture".equals(uri.getAuthority())) removed=documents.remove(DocumentsContract.getDocumentId(uri));
        if (removed!=null) {data.remove(uri.toString());deletes++;return 1;}return 0;
    }
    public void takePersistableUriPermission(Uri uri,int flags) { }
    public Cursor query(Uri uri,String[] projection,String selection,String[] args,String sort) {
        if (failQuery) throw new IllegalStateException("fixture query failure");
        if ("media".equals(uri.getAuthority())) {
            MatrixCursor cursor=new MatrixCursor(projection,4);
            ownershipQueried=selection!=null && selection.contains("owner_package_name") && selection.contains("relative_path");
            for (java.util.Map.Entry<String,ContentValues> entry:mediaRows.entrySet()) {
                if (!uri.toString().equals("content://media/external_primary/images/media") && !entry.getKey().equals(uri.toString())) continue;
                ContentValues row=entry.getValue();
                if (args!=null && (!args[0].equals(row.get("relative_path")) || !args[1].equals(row.get("owner_package_name")) || (args.length>2 && !args[2].equals(row.get("_display_name"))))) continue;
                Object[] values=new Object[projection.length];for(int i=0;i<projection.length;i++) values[i]=row.get(projection[i]);cursor.addRow(values);
            }
            return cursor;
        }
        if ("fixture".equals(uri.getAuthority()) && uri.toString().contains("/tree/")) {
            MatrixCursor cursor=new MatrixCursor(projection,4);String id=DocumentsContract.getDocumentId(uri);
            boolean children=uri.toString().contains("/children/");
            for (java.util.Map.Entry<String,ContentValues> entry:documents.entrySet()) {
                ContentValues row=entry.getValue();if (children?!id.equals(row.get("parent")):!id.equals(entry.getKey())) continue;
                Object[] values=new Object[projection.length];for(int i=0;i<projection.length;i++) values[i]=row.get(projection[i]);cursor.addRow(values);
            }
            return cursor;
        }
        return provider.query(uri,projection,selection,args,sort);
    }
    public String getType(Uri uri) { return provider==null?null:provider.getType(uri); }
    public InputStream openInputStream(Uri uri) throws IOException {
        byte[] bytes=data.get(uri.toString());if(bytes==null) throw new IOException("No input fixture");return new ByteArrayInputStream(bytes);
    }
    public OutputStream openOutputStream(Uri uri,String mode) throws IOException {
        if (failOutput) throw new IOException("fixture destination failure");
        ContentValues row=mediaRows.get(uri.toString());
        if (row!=null && !Integer.valueOf(1).equals(row.get("is_pending"))) throw new AssertionError("Image exposed before copy");
        boolean stored=row!=null || ("fixture".equals(uri.getAuthority()) && documents.containsKey(DocumentsContract.getDocumentId(uri)));
        if (stored) data.put(uri.toString(),new byte[0]);
        exported=new ByteArrayOutputStream();final ByteArrayOutputStream buffer=exported;
        return new OutputStream() {
            public void write(int value) throws IOException {write(new byte[]{(byte)value},0,1);}
            public void write(byte[] bytes,int start,int count) throws IOException {
                if (duringWrite!=null) {Runnable callback=duringWrite;duringWrite=null;callback.run();}
                if (failWriteOnce) {failWriteOnce=false;buffer.write(bytes,start,Math.min(2,count));if(stored)data.put(uri.toString(),buffer.toByteArray());throw new IOException("fixture image write failure");}
                if (failWrite) throw new IOException("fixture image write failure");
                buffer.write(bytes,start,count);
            }
            public void close() throws IOException {
                if (stored) {data.put(uri.toString(),buffer.toByteArray());ContentValues metadata=mediaRows.get(uri.toString());if(metadata==null)metadata=documents.get(DocumentsContract.getDocumentId(uri));if(metadata!=null)metadata.put("_size",(long)buffer.size());}
                if (failCloseOnce) {failCloseOnce=false;throw new IOException("fixture image close failure");}
                if (failClose) throw new IOException("fixture image close failure");
            }
        };
    }
    public Uri createDocument(Uri parent,String mime,String name) {
        ContentValues row=new ContentValues();String id="doc-"+(++nextDocument);
        row.put("document_id",id);row.put("parent",DocumentsContract.getDocumentId(parent));row.put("_display_name",name);row.put("mime_type",mime);row.put("_size",0L);documents.put(id,row);
        Uri uri=DocumentsContract.buildDocumentUriUsingTree(parent,id);data.put(uri.toString(),new byte[0]);return uri;
    }
    public Uri renameDocument(Uri uri,String name) {
        ContentValues row=documents.get(DocumentsContract.getDocumentId(uri));if(row==null)return null;
        row.put("_display_name",name);return uri;
    }
}
