package android.app;
public class Notification {
    public String channel,title,text;
    public int smallIcon;
    public boolean ongoing,showWhen;
    public PendingIntent contentIntent;
    public static class Builder {
        private final Notification value=new Notification();
        public Builder(android.content.Context context,String channel){value.channel=channel;}
        public Builder setContentTitle(CharSequence title){value.title=title.toString();return this;}
        public Builder setContentText(CharSequence text){value.text=text.toString();return this;}
        public Builder setSmallIcon(int icon){value.smallIcon=icon;return this;}
        public Builder setOngoing(boolean value){this.value.ongoing=value;return this;}
        public Builder setShowWhen(boolean value){this.value.showWhen=value;return this;}
        public Builder setContentIntent(PendingIntent intent){value.contentIntent=intent;return this;}
        public Notification build(){return value;}
    }
}
