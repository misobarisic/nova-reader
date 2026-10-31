package android.database;
public interface Cursor extends AutoCloseable {
    boolean moveToFirst();
    boolean moveToNext();
    String getString(int column);
    long getLong(int column);
    boolean isNull(int column);
    void close();
}
