package android.content;
import android.net.Uri;
public class ContentUris {
    public static Uri withAppendedId(Uri uri,long id) { return Uri.parse(uri.toString()+"/"+id); }
}
