package android.content;
import android.net.Uri;
public class Intent {
    public static final String ACTION_VIEW="android.intent.action.VIEW";
    public static final String ACTION_OPEN_DOCUMENT="android.intent.action.OPEN_DOCUMENT";
    public static final String ACTION_OPEN_DOCUMENT_TREE="android.intent.action.OPEN_DOCUMENT_TREE";
    public static final String ACTION_CREATE_DOCUMENT="android.intent.action.CREATE_DOCUMENT";
    public static final String CATEGORY_OPENABLE="android.intent.category.OPENABLE";
    public static final String CATEGORY_BROWSABLE="android.intent.category.BROWSABLE";
    public static final String EXTRA_TITLE="android.intent.extra.TITLE";
    public static final String EXTRA_MIME_TYPES="android.intent.extra.MIME_TYPES";
    public static final int FLAG_GRANT_READ_URI_PERMISSION=1;
    public static final int FLAG_GRANT_WRITE_URI_PERMISSION=2;
    public static final int FLAG_GRANT_PERSISTABLE_URI_PERMISSION=64;
    public static final int FLAG_GRANT_PREFIX_URI_PERMISSION=128;
    public static final int FLAG_ACTIVITY_NEW_TASK=0x10000000;
    public static final int FLAG_ACTIVITY_SINGLE_TOP=0x20000000;
    public Class<?> serviceClass;
    public final String action;
    public String type;
    public String title;
    private Uri data;
    private int flags;
    private final java.util.Set<String> categories=new java.util.HashSet<>();
    public Intent(String action) { this.action=action; }
    public Intent(Context context,Class<?> type) {this.action="fixture-service";this.serviceClass=type;}
    public Intent(String action,Uri data) { this(action);this.data=data; }
    public Intent setType(String type) { this.type=type; return this; }
    public Intent addFlags(int flags) { this.flags|=flags; return this; }
    public Intent addCategory(String category) { categories.add(category);return this; }
    public boolean hasCategory(String category) { return categories.contains(category); }
    public Intent putExtra(String key,String value) { if (EXTRA_TITLE.equals(key)) title=value;return this; }
    public Intent putExtra(String key,String[] value) { return this; }
    public int getFlags() { return flags; }
    public Intent setData(Uri data) { this.data=data;return this; }
    public Uri getData() { return data; }
}
