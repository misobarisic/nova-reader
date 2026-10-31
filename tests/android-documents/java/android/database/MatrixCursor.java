package android.database;
public class MatrixCursor implements Cursor {
    private Object[] row;
    public MatrixCursor(String[] columns,int capacity) { }
    public void addRow(Object[] values) { row=values; }
    public boolean moveToFirst() { return row!=null; }
    public boolean moveToNext() { return false; }
    public String getString(int column) { return (String)row[column]; }
    public long getLong(int column) { return ((Number)row[column]).longValue(); }
    public boolean isNull(int column) { return row[column]==null; }
    public void close() { }
}
