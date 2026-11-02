package dev.misob.nova.reader;

import android.app.Application;
import android.app.Notification;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.content.pm.ServiceInfo;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.PowerManager;

/** Runs the unmodified Nova service recipe and Reader's actual permission host. */
public final class BackgroundServiceFixture {
    private static void check(boolean condition,String message) {
        if (!condition) throw new AssertionError(message);
    }
    private static ReaderActivity activity(boolean resume) {
        ReaderActivity activity=new ReaderActivity();
        activity.onCreate(new Bundle());
        if (resume) activity.onResume();
        return activity;
    }
    private static void notification(Notification notification,String title,String text) {
        check("reader.background".equals(notification.channel),"Reader-specific notification channel");
        check(title.equals(notification.title)&&text.equals(notification.text),"notification progress text");
        check(notification.ongoing&&!notification.showWhen,"ongoing notification without a false timestamp");
        check(notification.smallIcon==android.R.drawable.stat_sys_download,"platform download icon");
        check(notification.contentIntent!=null,"notification opens Reader");
        check((notification.contentIntent.flags&PendingIntent.FLAG_IMMUTABLE)!=0,"immutable notification intent");
        check((notification.contentIntent.flags&PendingIntent.FLAG_UPDATE_CURRENT)!=0,"update existing notification intent");
        check((notification.contentIntent.intent.getFlags()&Intent.FLAG_ACTIVITY_SINGLE_TOP)!=0,"reuse active Reader activity");
    }
    private static void service(int api) {
        Build.VERSION.SDK_INT=api;
        Context application=new Application();
        int notifications=Context.notifications.updates;
        ReaderBackgroundService.update(application,"inactive","not delivered");
        check(Context.notifications.updates==notifications,"updates before service creation are ignored");
        int starts=Context.foregroundStarts;
        ReaderBackgroundService.start(application);
        check(Context.foregroundStarts==starts+1,"API26+ uses startForegroundService");
        check(Context.serviceIntent.serviceClass==ReaderBackgroundService.class,"Reader service component");
        Handler.drain(); // A headless Application does not own permission UI.
        ReaderBackgroundService service=new ReaderBackgroundService();
        service.onCreate();
        check(service.creates==1,"super.onCreate is called");
        check(Context.notifications.channel.id.equals("reader.background"),"download channel exists");
        check(Context.notifications.channel.importance==NotificationManager.IMPORTANCE_LOW,"low-priority download channel");
        check(Context.power.lastLevel==PowerManager.PARTIAL_WAKE_LOCK,"screen-off CPU wake lock");
        PowerManager.WakeLock lock=Context.power.last;
        check(lock.held&&!lock.referenceCounted&&lock.acquires==1,"one non-reference-counted wake lock");
        check("reader:downloads".equals(lock.tag),"Reader-specific wake lock tag");
        int result=service.onStartCommand(Context.serviceIntent,0,1);
        check(result==Service.START_NOT_STICKY,"no service-only restart without Rust workers");
        check(service.typedForeground==(api>=29),"API29+ dataSync foreground type");
        if(api>=29) check(service.foregroundType==ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC,"dataSync type");
        notification(service.notification,"Downloading","Preparing…");
        int id=service.foregroundId;
        ReaderBackgroundService.update(application,"Fixture chapter","2 / 4 pages");
        check(Context.notifications.lastId==id,"updates replace the foreground notification");
        notification(Context.notifications.notification,"Fixture chapter","2 / 4 pages");
        service.onStartCommand(null,0,2);
        check(lock.acquires==1,"repeat start does not acquire another wake lock");
        check(service.onBind(null)==null,"service has no binding interface");
        if(api>=35) {
            service.onTimeout(2,ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC);
            check(service.stops==1,"Android15 dataSync timeout stops the service");
            check(lock.held,"Android calls onDestroy to release the timeout wake lock");
        }
        int stops=Context.serviceStops;
        ReaderBackgroundService.stop(application);
        check(Context.serviceStops==stops+1,"stop is delegated to application Context");
        service.onDestroy();
        check(service.destroys==1&&!lock.held&&lock.releases==1,"destroy releases wake lock and calls super");
        notifications=Context.notifications.updates;
        ReaderBackgroundService.update(application,"gone","not delivered");
        check(Context.notifications.updates==notifications,"weak instance cleared on destroy");
        service.onDestroy();
        check(lock.releases==1,"repeated cleanup cannot release the lock twice");
        ReaderBackgroundService.stop(application); // Safe when Android reports no running service.
    }
    private static void permission() {
        Build.VERSION.SDK_INT=35;
        ReaderActivity.requestNotificationPermission(41);Handler.drain();
        ReaderActivity created=activity(false);
        ReaderActivity.requestNotificationPermission(42);Handler.drain();
        check(created.permissionRequests==0,"created but not resumed activity cannot own permission UI");
        created.onResume();
        Build.VERSION.SDK_INT=29;
        ReaderActivity.requestNotificationPermission(43);Handler.drain();
        check(created.permissionRequests==0,"pre-33 needs no runtime notification permission");
        Build.VERSION.SDK_INT=35;
        ReaderActivity.requestNotificationPermission(44);
        check(created.permissionRequests==0,"permission request is posted to Android main");
        Handler.drain();
        check(created.permissionRequests==1&&created.permissionCode==44,"resumed activity receives requested code");
        check(created.requestedPermissions.length==1&&"android.permission.POST_NOTIFICATIONS".equals(created.requestedPermissions[0]),"notification permission only");
        check("ReaderFixtureMain".equals(created.permissionThread),"permission UI runs on main queue");
        created.permissionGranted=true;
        ReaderActivity.requestNotificationPermission(45);Handler.drain();
        check(created.permissionRequests==1,"already-granted permission is not requested again");
        created.permissionGranted=false;
        ReaderActivity.requestNotificationPermission(46);created.onPause();Handler.drain();
        check(created.permissionRequests==1,"queued request cannot launch after pause");
        created.onResume();ReaderActivity.requestNotificationPermission(47);created.onPause();
        ReaderActivity replacement=activity(true);created.onDestroy();Handler.drain();
        check(created.permissionRequests==1&&replacement.permissionRequests==1&&replacement.permissionCode==47,"resolve latest resumed Activity at dispatch, preserving recreation");
        replacement.finishing=true;
        ReaderActivity.requestNotificationPermission(48);Handler.drain();
        check(replacement.permissionRequests==1,"finishing activity cannot open permission UI");
        replacement.finishing=false;
        ReaderActivity.requestNotificationPermission(49);replacement.onDestroy();Handler.drain();
        check(replacement.permissionRequests==1,"destroyed activity cannot open permission UI");
        // Denied notification permission never prevents the required service
        // start; permission prompting and download lifetime are independent.
        ReaderActivity resumed=activity(true);int starts=Context.foregroundStarts;
        ReaderBackgroundService.start(new Application());
        check(Context.foregroundStarts==starts+1,"permission denial does not gate foreground service");
        Handler.drain();
        check(resumed.permissionCode==0x53,"copied Nova service permission request code");
        resumed.onPause();resumed.onDestroy();
    }
    private static void optionalServices() {
        Build.VERSION.SDK_INT=35;
        PackageManager.launchAvailable=false;
        Context.powerAvailable=false;Context.notificationAvailable=false;
        ReaderBackgroundService service=new ReaderBackgroundService();service.onCreate();
        service.onStartCommand(null,0,1);
        check(service.notification.contentIntent==null,"missing launch intent is optional");
        ReaderBackgroundService.update(new Application(),"no manager","no crash");
        service.onDestroy();
        Context.powerAvailable=true;Context.notificationAvailable=true;PackageManager.launchAvailable=true;
        Build.VERSION.SDK_INT=25;
        int starts=Context.ordinaryStarts;ReaderBackgroundService.start(new Application());
        check(Context.ordinaryStarts==starts+1,"copied legacy startService branch");
    }
    // JNI harness entry points: use the real retained Application and class
    // loader while the originating Activity is paused/destroyed.
    private static ReaderActivity nativeActivity;
    private static ReaderBackgroundService nativeService;
    public static ReaderActivity bootstrapActivity() {Build.VERSION.SDK_INT=35;nativeActivity=activity(true);return nativeActivity;}
    public static void dropActivity() {nativeActivity.onPause();nativeActivity.onDestroy();nativeActivity=null;}
    public static void createService() {nativeService=new ReaderBackgroundService();nativeService.onCreate();nativeService.onStartCommand(Context.serviceIntent,0,1);}
    public static void destroyService() {if(nativeService!=null){nativeService.onDestroy();nativeService=null;}}
    public static int foregroundStarts() {return Context.foregroundStarts;}
    public static int serviceStops() {return Context.serviceStops;}
    public static int notificationUpdates() {return Context.notifications.updates;}
    public static int classLoads() {return Context.classLoads;}
    public static boolean applicationOwnsService() {return Context.serviceContext instanceof Application;}
    public static String notificationTitle() {return Context.notifications.notification.title;}
    public static String notificationText() {return Context.notifications.notification.text;}
    public static void failStartOnce() {Context.failForegroundStartOnce=true;}
    public static void drain() {Handler.drain();}
    public static void main(String[] args) {
        service(29);service(35);service(28);permission();optionalServices();
        System.out.println("Reader background service API29/35 lifetime, notification, wake lock, timeout and resumed-only permission regressions passed");
    }
}
