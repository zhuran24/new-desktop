/* Only the private native test compositor can receive these events. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <wayland-client.h>
#include <libei.h>
#include <poll.h>
#include "fake-input.h"

static struct org_kde_kwin_fake_input *input;
static struct ei_device *wheel;
static void global(void *data, struct wl_registry *registry, uint32_t name,
                   const char *interface, uint32_t version) {
    (void)data;
    if (!strcmp(interface, "org_kde_kwin_fake_input") && version >= 5)
        input = wl_registry_bind(registry, name, &org_kde_kwin_fake_input_interface, 5);
}
static void removed(void *data, struct wl_registry *registry, uint32_t name) {
    (void)data; (void)registry; (void)name;
}
static const struct wl_registry_listener listener = {global, removed};
static void events(struct ei *sender) {
    ei_dispatch(sender);
    struct ei_event *event;
    while ((event = ei_get_event(sender))) {
        if (ei_event_get_type(event) == EI_EVENT_SEAT_ADDED)
            ei_seat_bind_capabilities(ei_event_get_seat(event), EI_DEVICE_CAP_POINTER, EI_DEVICE_CAP_SCROLL, NULL);
        if (ei_event_get_type(event) == EI_EVENT_DEVICE_RESUMED && !wheel &&
            ei_device_has_capability(ei_event_get_device(event), EI_DEVICE_CAP_SCROLL)) {
            wheel = ei_device_ref(ei_event_get_device(event));
            ei_device_start_emulating(wheel, 1);
        }
        ei_event_unref(event);
    }
}
int main(int argc, char **argv) {
    const char *runtime = getenv("XDG_RUNTIME_DIR"), *socket = getenv("WAYLAND_DISPLAY");
    if (argc != 2 || !runtime || strcmp(runtime, "/sandbox/runtime") ||
        !socket || strcmp(socket, "nd-test-header") || getenv("WAYLAND_SOCKET")) return 2;
    struct wl_display *display = wl_display_connect(NULL);
    if (!display) return 3;
    struct wl_registry *registry = wl_display_get_registry(display);
    wl_registry_add_listener(registry, &listener, NULL);
    wl_display_roundtrip(display);
    if (!input) return 4;
    org_kde_kwin_fake_input_authenticate(input, "New Desktop native test", "private KWin only");
    wl_display_roundtrip(display);
    struct ei *sender = ei_new_sender(NULL);
    ei_configure_name(sender, "nd-test-header-wheel");
    if (ei_setup_backend_fd(sender, atoi(argv[1]))) return 5;
    for (int i = 0; !wheel && i < 100; i++) {
        struct pollfd fd = {ei_get_fd(sender), POLLIN, 0};
        poll(&fd, 1, 50); events(sender);
    }
    if (!wheel) return 6;
    char kind[16]; double a, b;
    while (scanf("%15s %lf %lf", kind, &a, &b) == 3) {
        if (!strcmp(kind, "motion"))
            org_kde_kwin_fake_input_pointer_motion_absolute(input, wl_fixed_from_double(a), wl_fixed_from_double(b));
        else if (!strcmp(kind, "button") && a == 272 && (b == 0 || b == 1))
            org_kde_kwin_fake_input_button(input, (uint32_t)a, (uint32_t)b);
        else if (!strcmp(kind, "key") && a >= 0 && a <= 57 && (b == 0 || b == 1))
            org_kde_kwin_fake_input_keyboard_key(input, (uint32_t)a, (uint32_t)b);
        else if (!strcmp(kind, "wheel")) {
            ei_device_scroll_discrete(wheel, 0, (int32_t)b);
            ei_device_frame(wheel, ei_now(sender)); events(sender);
        } else break;
        wl_display_roundtrip(display);
        puts("ok"); fflush(stdout);
    }
    ei_device_stop_emulating(wheel); ei_device_unref(wheel); ei_unref(sender);
    org_kde_kwin_fake_input_destroy(input);
    wl_registry_destroy(registry); wl_display_disconnect(display);
    return 0;
}
