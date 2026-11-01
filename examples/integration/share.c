/* Open the Nyx Share Sheet for files from C, with GLib's GDBus only.
 *
 *   cc share.c -o share $(pkg-config --cflags --libs gio-unix-2.0 json-glib-1.0)
 *   ./share FILE...
 *
 * Files are opened read-only and passed as descriptors (a Unix FD list), so
 * the service never needs the caller's paths.
 */
#include <fcntl.h>
#include <gio/gio.h>
#include <gio/gunixfdlist.h>
#include <json-glib/json-glib.h>
#include <stdio.h>
#include <string.h>

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s FILE...\n", argv[0]);
        return 2;
    }
    g_autoptr(GError) error = NULL;
    g_autoptr(GUnixFDList) fds = g_unix_fd_list_new();
    g_autoptr(JsonBuilder) items = json_builder_new();
    json_builder_begin_array(items);
    for (int i = 1; i < argc; i++) {
        int fd = open(argv[i], O_RDONLY | O_CLOEXEC);
        if (fd < 0) {
            perror(argv[i]);
            return 1;
        }
        int index = g_unix_fd_list_append(fds, fd, &error);
        close(fd);
        if (index < 0) {
            fprintf(stderr, "%s\n", error->message);
            return 1;
        }
        g_autofree char *name = g_path_get_basename(argv[i]);
        json_builder_begin_object(items);
        json_builder_set_member_name(items, "kind");
        json_builder_add_string_value(items, "file");
        json_builder_set_member_name(items, "fd");
        json_builder_add_int_value(items, index);
        json_builder_set_member_name(items, "name");
        json_builder_add_string_value(items, name);
        json_builder_end_object(items);
    }
    json_builder_end_array(items);
    g_autoptr(JsonGenerator) gen = json_generator_new();
    g_autoptr(JsonNode) root = json_builder_get_root(items);
    json_generator_set_root(gen, root);
    g_autofree char *json = json_generator_to_data(gen, NULL);

    /* The handles in the message are indexes into the FD list. */
    GVariantBuilder handles;
    g_variant_builder_init(&handles, G_VARIANT_TYPE("ah"));
    for (int i = 0; i < g_unix_fd_list_get_length(fds); i++)
        g_variant_builder_add(&handles, "h", i);

    g_autoptr(GDBusConnection) bus = g_bus_get_sync(G_BUS_TYPE_SESSION, NULL, &error);
    if (!bus) {
        fprintf(stderr, "%s\n", error->message);
        return 1;
    }
    /* Share(s items, ah fds, s options) → s request id */
    g_autoptr(GVariant) reply = g_dbus_connection_call_with_unix_fd_list_sync(
        bus, "io.github.design_nexus.nyx.Share1", "/io/github/design_nexus/nyx/Share1",
        "io.github.design_nexus.nyx.Share1", "Share",
        g_variant_new("(sahs)", json, &handles, "{\"app_name\":\"C example\"}"), G_VARIANT_TYPE("(s)"),
        G_DBUS_CALL_FLAGS_NONE, -1, fds, NULL, NULL, &error);
    if (!reply) {
        /* The message is a sentence that can be shown as it is. */
        g_dbus_error_strip_remote_error(error);
        fprintf(stderr, "Couldn't share: %s\n", error->message);
        return 1;
    }
    const char *request;
    g_variant_get(reply, "(&s)", &request);
    printf("%s\n", request);
    return 0;
}
