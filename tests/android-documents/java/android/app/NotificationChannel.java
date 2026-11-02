package android.app;
public class NotificationChannel {
    public final String id,name;
    public final int importance;
    public String description;
    public NotificationChannel(String id,CharSequence name,int importance){this.id=id;this.name=name.toString();this.importance=importance;}
    public void setDescription(String description){this.description=description;}
}
