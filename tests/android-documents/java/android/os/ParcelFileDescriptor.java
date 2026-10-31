package android.os;
import java.io.File;
import java.io.FileDescriptor;
import java.io.FileInputStream;
import java.io.FileNotFoundException;
import java.io.IOException;
public class ParcelFileDescriptor implements AutoCloseable {
    public static final int MODE_READ_ONLY=268435456;
    private final FileInputStream input;
    private ParcelFileDescriptor(File file) throws FileNotFoundException { input=new FileInputStream(file); }
    public static ParcelFileDescriptor open(File file,int mode) throws FileNotFoundException {
        if (mode!=MODE_READ_ONLY) throw new AssertionError("Writable clipboard descriptor");
        return new ParcelFileDescriptor(file);
    }
    public FileDescriptor getFileDescriptor() throws IOException { return input.getFD(); }
    public void close() throws IOException { input.close(); }
}
