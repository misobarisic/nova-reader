package android.database;
public class MatrixCursor implements Cursor {
    private final java.util.List<Object[]> rows=new java.util.ArrayList<>();
    private int index=-1;
    public MatrixCursor(String[] columns,int capacity) { }
    public void addRow(Object[] values) { rows.add(values); }
    public boolean moveToFirst() { index=0;return !rows.isEmpty(); }
    public boolean moveToNext() { return ++index<rows.size(); }
    public String getString(int column) { return (String)rows.get(index)[column]; }
    public long getLong(int column) { return ((Number)rows.get(index)[column]).longValue(); }
    public boolean isNull(int column) { return rows.get(index)[column]==null; }
    public void close() { }
}
