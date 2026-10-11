/* Test-only input goes through a private KWin, never through /dev/uinput. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <libei.h>
#include <poll.h>
#include <wayland-client.h>
#include "fake-input.h"

static struct org_kde_kwin_fake_input *input;
static struct ei_device *wheel;
static void dispatch(struct ei *sender) {
    ei_dispatch(sender);
    struct ei_event *event;
    while ((event = ei_get_event(sender))) {
        if (ei_event_get_type(event) == EI_EVENT_SEAT_ADDED)
            ei_seat_bind_capabilities(ei_event_get_seat(event), EI_DEVICE_CAP_POINTER,
                                     EI_DEVICE_CAP_SCROLL, NULL);
        if (ei_event_get_type(event) == EI_EVENT_DEVICE_RESUMED) {
            struct ei_device *device = ei_event_get_device(event);
            if (!wheel && ei_device_has_capability(device, EI_DEVICE_CAP_SCROLL)) {
                wheel = ei_device_ref(device);
                ei_device_start_emulating(wheel, 1);
            }
        }
        ei_event_unref(event);
    }
}
static void global(void *data, struct wl_registry *registry, uint32_t id,
                   const char *name, uint32_t version) {
    (void)data;
    if (!strcmp(name, "org_kde_kwin_fake_input"))
        input = wl_registry_bind(registry, id, &org_kde_kwin_fake_input_interface,
                                 version < 4 ? version : 4);
}
static void removed(void *data, struct wl_registry *registry, uint32_t id) {
    (void)data; (void)registry; (void)id;
}
static const struct wl_registry_listener listener = { global, removed };

int main(int argc, char **argv) {
    const char *runtime = getenv("XDG_RUNTIME_DIR"), *socket = getenv("WAYLAND_DISPLAY");
    if ((argc != 1 && argc != 2) || !runtime || strcmp(runtime, "/sandbox/runtime") ||
        !socket || strcmp(socket, "nd-test-native") || getenv("WAYLAND_SOCKET"))
        return 2;
    struct wl_display *display = wl_display_connect(NULL);
    if (!display) return 3;
    struct wl_registry *registry = wl_display_get_registry(display);
    wl_registry_add_listener(registry, &listener, NULL);
    if (wl_display_roundtrip(display) < 0 || !input) return 4;
    org_kde_kwin_fake_input_authenticate(input, "New Desktop #66 regression", "private KWin only");
    if (wl_display_roundtrip(display) < 0) return 5;
    /* Discrete wheel identity is supplied by EIS; fake-input axis lacks it. */
    struct ei *sender = NULL;
    if (argc == 2) {
        sender = ei_new_sender(NULL);
        ei_configure_name(sender, "nd-test-private-ime-wheel");
        if (ei_setup_backend_fd(sender, atoi(argv[1]))) return 8;
        for (int n = 0; !wheel && n < 100; n++) {
            struct pollfd fd = {ei_get_fd(sender), POLLIN, 0};
            poll(&fd, 1, 50); dispatch(sender);
        }
        if (!wheel) return 9;
        puts("ready"); fflush(stdout);
    }
    char kind[16]; double code, value;
    while (scanf("%15s %lf %lf", kind, &code, &value) == 3) {
        if (!strcmp(kind, "motion"))
            org_kde_kwin_fake_input_pointer_motion_absolute(input, wl_fixed_from_double(code), wl_fixed_from_double(value));
        else if (!strcmp(kind, "button") && code == 272 && (value == 0 || value == 1))
            org_kde_kwin_fake_input_button(input, (uint32_t)code, (uint32_t)value);
        else if (!strcmp(kind, "wheel") && code == 0 && wheel && value >= -1200 && value <= 1200) {
            ei_device_scroll_discrete(wheel, 0, (int32_t)value);
            ei_device_frame(wheel, ei_now(sender)); dispatch(sender);
        }
        else if (!strcmp(kind, "key") && code >= 0 && code < 256 && (value == 0 || value == 1))
            org_kde_kwin_fake_input_keyboard_key(input, (uint32_t)code, (uint32_t)value);
        else return 6;
        if (wl_display_roundtrip(display) < 0) return 7;
        puts("ok"); fflush(stdout);
    }
    org_kde_kwin_fake_input_destroy(input);
    wl_registry_destroy(registry);
    wl_display_disconnect(display);
    if (wheel) {
        ei_device_stop_emulating(wheel);
        ei_device_unref(wheel); ei_unref(sender);
    }
    return 0;
}
