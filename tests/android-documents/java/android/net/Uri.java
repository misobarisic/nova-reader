package android.net;
import java.util.Arrays;
import java.util.Collections;
import java.util.List;
public class Uri {
    private final java.net.URI value;
    private Uri(String text) { value=java.net.URI.create(text); }
    public static Uri parse(String text) { return new Uri(text); }
    public String getScheme() { return value.getScheme(); }
    public String getAuthority() { return value.getAuthority(); }
    public List<String> getPathSegments() {
        String path=value.getPath();
        return path==null || path.isEmpty()?Collections.emptyList():Arrays.asList(path.substring(1).split("/",-1));
    }
    @Override public String toString() { return value.toString(); }
    public static class Builder {
        private String scheme,authority,path;
        public Builder scheme(String value) { scheme=value;return this; }
        public Builder authority(String value) { authority=value;return this; }
        public Builder appendPath(String value) { path=value;return this; }
        public Uri build() { return parse(scheme+"://"+authority+"/"+path); }
    }
}
