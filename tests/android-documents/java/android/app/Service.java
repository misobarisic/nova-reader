package android.app;
public class Service extends android.content.Context {
    public static final int START_NOT_STICKY=2;
    private static final Application APPLICATION=new Application();
    @Override public android.content.Context getApplicationContext() {return APPLICATION;}
    public int creates,destroys,foregroundCalls,foregroundId,foregroundType,stops;
    public boolean typedForeground;
    public Notification notification;
    public void onCreate(){creates++;}
    public int onStartCommand(android.content.Intent intent,int flags,int id){return START_NOT_STICKY;}
    public void onTimeout(int startId,int type){ }
    public void onDestroy(){destroys++;}
    public android.os.IBinder onBind(android.content.Intent intent){return null;}
    public void startForeground(int id,Notification value){foregroundCalls++;foregroundId=id;notification=value;typedForeground=false;}
    public void startForeground(int id,Notification value,int type){startForeground(id,value);foregroundType=type;typedForeground=true;}
    public void stopSelf(){stops++;}
}
