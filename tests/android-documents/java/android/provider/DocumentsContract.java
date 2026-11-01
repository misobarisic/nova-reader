package android.provider;
import android.content.ContentResolver;
import android.net.Uri;
public class DocumentsContract {
    public static boolean isTreeUri(Uri uri) { return uri.getPathSegments().contains("tree"); }
    public static class Document {
        public static final String COLUMN_DOCUMENT_ID="document_id";
        public static final String COLUMN_DISPLAY_NAME="_display_name";
        public static final String COLUMN_MIME_TYPE="mime_type";
        public static final String COLUMN_SIZE="_size";
        public static final String MIME_TYPE_DIR="vnd.android.document/directory";
    }
    public static String getTreeDocumentId(Uri uri) { java.util.List<String> path=uri.getPathSegments();return path.get(path.indexOf("tree")+1); }
    public static String getDocumentId(Uri uri) { java.util.List<String> path=uri.getPathSegments();return path.get(path.size()-1); }
    public static Uri buildDocumentUriUsingTree(Uri tree,String id) { return Uri.parse("content://fixture/tree/"+getTreeDocumentId(tree)+"/document/"+id); }
    public static Uri buildChildDocumentsUriUsingTree(Uri tree,String id) { return Uri.parse("content://fixture/tree/"+getTreeDocumentId(tree)+"/children/"+id); }
    public static Uri createDocument(ContentResolver resolver,Uri parent,String mime,String name) { return resolver.createDocument(parent,mime,name); }
    public static Uri renameDocument(ContentResolver resolver,Uri uri,String name) { return resolver.renameDocument(uri,name); }
    public static boolean deleteDocument(ContentResolver resolver,Uri uri) { return resolver.delete(uri,null,null)>0; }
}
