package android.app;
public class Application extends android.content.Context {
    public static boolean failLoader;
    @Override public ClassLoader getClassLoader() {
        if (failLoader) throw new IllegalStateException("fixture class loader failure");
        return super.getClassLoader();
    }
}
