package android.content;
public class ContentValues extends java.util.HashMap<String,Object> {
    public ContentValues() { }
    public ContentValues(ContentValues other) { super(other); }
    public void put(String key,String value) { super.put(key,value); }
    public void put(String key,Integer value) { super.put(key,value); }
    public String getAsString(String key) { return (String)get(key); }
    public Integer getAsInteger(String key) { return (Integer)get(key); }
}
