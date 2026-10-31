package android.view;
public class Window {
    private final View decor=new View();
    private final WindowInsetsController controller=new WindowInsetsController();
    public View getDecorView() { return decor; }
    public WindowInsetsController getInsetsController() { return controller; }
}
