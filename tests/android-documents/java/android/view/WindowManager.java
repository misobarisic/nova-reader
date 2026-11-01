package android.view;
public interface WindowManager {
    class LayoutParams {
        public static final int FLAG_KEEP_SCREEN_ON=128;
        public static final int FLAG_FULLSCREEN=1024;
        public static final int FLAG_TRANSLUCENT_STATUS=0x04000000;
        public static final int FLAG_DRAWS_SYSTEM_BAR_BACKGROUNDS=0x80000000;
        public static final int LAYOUT_IN_DISPLAY_CUTOUT_MODE_DEFAULT=0;
        public static final int LAYOUT_IN_DISPLAY_CUTOUT_MODE_NEVER=2;
        public static final int LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES=1;
        public static final int LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS=3;
        public int layoutInDisplayCutoutMode;
        public int flags=FLAG_KEEP_SCREEN_ON|FLAG_TRANSLUCENT_STATUS|FLAG_FULLSCREEN;
    }
}
