package android.provider;
import android.net.Uri;
public class MediaStore {
    public static final String VOLUME_EXTERNAL_PRIMARY="external_primary";
    public static class Images {
        public static class Media {
            public static final String DISPLAY_NAME="_display_name";
            public static final String MIME_TYPE="mime_type";
            public static final String RELATIVE_PATH="relative_path";
            public static final String IS_PENDING="is_pending";
            public static Uri getContentUri(String volume) { return Uri.parse("content://media/"+volume+"/images/media"); }
        }
    }
}
