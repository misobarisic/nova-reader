package android.app;
public class PendingIntent {
    public static final int FLAG_UPDATE_CURRENT=0x08000000,FLAG_IMMUTABLE=0x04000000;
    public android.content.Intent intent;
    public int flags,requestCode;
    public static PendingIntent getActivity(android.content.Context context,int request,android.content.Intent intent,int flags){PendingIntent value=new PendingIntent();value.intent=intent;value.flags=flags;value.requestCode=request;return value;}
}
