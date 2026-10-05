// A toolkit window used only to verify the isolated capture harness.
#include <gtk/gtk.h>

static void activate(GtkApplication *app, gpointer unused) {
    (void)unused;
    GtkWidget *window = gtk_application_window_new(app);
    gtk_window_set_title(GTK_WINDOW(window), "Native validation toolkit probe");
    gtk_window_set_default_size(GTK_WINDOW(window), 480, 240);
    gtk_window_set_child(GTK_WINDOW(window), gtk_button_new_with_label("Capture tooling ready"));
    gtk_window_present(GTK_WINDOW(window));
    int dpi;
    char *font;
    g_object_get(gtk_settings_get_default(), "gtk-xft-dpi", &dpi, "gtk-font-name", &font, NULL);
    g_print("gtk_xft_dpi=%d\ngtk_font_name=%s\n", dpi, font);
    g_free(font);
}

int main(int argc, char **argv) {
    GtkApplication *app = gtk_application_new("local.crew.ValidationProbe", G_APPLICATION_DEFAULT_FLAGS);
    g_signal_connect(app, "activate", G_CALLBACK(activate), NULL);
    int result = g_application_run(G_APPLICATION(app), argc, argv);
    g_object_unref(app);
    return result;
}
