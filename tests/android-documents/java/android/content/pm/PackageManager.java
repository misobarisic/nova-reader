package android.content.pm;
public class PackageManager {
    public static final int PERMISSION_GRANTED=0,PERMISSION_DENIED=-1;
    public static boolean launchAvailable=true;
    public android.content.Intent getLaunchIntentForPackage(String name) {return launchAvailable?new android.content.Intent("fixture-launch"):null;}
}
