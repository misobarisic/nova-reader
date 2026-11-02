package android.os;
public class PowerManager {
    public static final int PARTIAL_WAKE_LOCK=1;
    public WakeLock last;
    public int requests,lastLevel;
    public WakeLock newWakeLock(int level,String tag) {requests++;lastLevel=level;last=new WakeLock(tag);return last;}
    public static class WakeLock {
        public final String tag;
        public boolean referenceCounted=true,held;
        public int acquires,releases;
        WakeLock(String tag){this.tag=tag;}
        public void setReferenceCounted(boolean value){referenceCounted=value;}
        public void acquire(){acquires++;held=true;}
        public boolean isHeld(){return held;}
        public void release(){if(!held)throw new AssertionError("wake lock released twice");releases++;held=false;}
    }
}
