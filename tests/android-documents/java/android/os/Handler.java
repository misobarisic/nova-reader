package android.os;
import java.util.ArrayDeque;
public class Handler {
    private static final ArrayDeque<Runnable> QUEUE=new ArrayDeque<>();
    public Handler(Looper looper) { }
    public boolean post(Runnable runnable) { QUEUE.add(runnable); return true; }
    // Execute on a foreign Java thread, so callback binding and Rust delivery
    // are tested outside the native thread that registered the method.
    public static void drain() {
        java.util.concurrent.atomic.AtomicReference<Throwable> failure=new java.util.concurrent.atomic.AtomicReference<>();
        Thread thread=new Thread(() -> {
            try { while (!QUEUE.isEmpty()) QUEUE.remove().run(); }
            catch (Throwable error) { failure.set(error); }
        },"ReaderFixtureMain");
        thread.start();
        try { thread.join(); }
        catch (InterruptedException error) { throw new AssertionError(error); }
        if (failure.get()!=null) throw new AssertionError(failure.get());
    }
}
