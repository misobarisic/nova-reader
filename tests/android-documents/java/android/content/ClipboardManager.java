package android.content;
public class ClipboardManager {
    public static ClipData primary;
    public static boolean failClipboard;
    public void setPrimaryClip(ClipData clip) {
        if (failClipboard) throw new IllegalStateException("fixture clipboard failure");
        primary=clip;
    }
}
