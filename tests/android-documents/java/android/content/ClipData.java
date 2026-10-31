package android.content;
import android.net.Uri;
public class ClipData {
    public final Uri uri;
    public final String mime;
    private ClipData(Uri uri,String mime) { this.uri=uri;this.mime=mime; }
    public static ClipData newUri(ContentResolver resolver,CharSequence label,Uri uri) {
        return new ClipData(uri,resolver.getType(uri));
    }
}
