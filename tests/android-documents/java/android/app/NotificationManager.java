package android.app;
public class NotificationManager {
    public static final int IMPORTANCE_LOW=2;
    public int creates,updates,lastId;
    public NotificationChannel channel;
    public Notification notification;
    public void createNotificationChannel(NotificationChannel channel){creates++;this.channel=channel;}
    public void notify(int id,Notification notification){updates++;lastId=id;this.notification=notification;}
}
