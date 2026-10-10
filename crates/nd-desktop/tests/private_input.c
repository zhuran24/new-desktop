/* Keyboard/pointer for the private compositor only; never creates uinput. */
#define _POSIX_C_SOURCE 200809L
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wayland-client.h>
#include "fake-input.h"

static struct org_kde_kwin_fake_input *input;
static void global(void *data, struct wl_registry *registry, uint32_t name,
                   const char *interface, uint32_t version) {
    (void)data;
    if (!strcmp(interface, "org_kde_kwin_fake_input"))
        input = wl_registry_bind(registry, name, &org_kde_kwin_fake_input_interface,
                                 version < 5 ? version : 5);
}
static void removed(void *data, struct wl_registry *registry, uint32_t name) {
    (void)data; (void)registry; (void)name;
}
static const struct wl_registry_listener listener = {global, removed};

int main(void) {
    const char *runtime = getenv("XDG_RUNTIME_DIR"), *socket = getenv("WAYLAND_DISPLAY");
    if (!runtime || strcmp(runtime, "/sandbox/runtime") || !socket ||
        strcmp(socket, "nd-test-idle-cpu") || getenv("WAYLAND_SOCKET")) return 2;
    struct wl_display *display = wl_display_connect(NULL);
    if (!display) return 3;
    struct wl_registry *registry = wl_display_get_registry(display);
    wl_registry_add_listener(registry, &listener, NULL);
    wl_display_roundtrip(display);
    if (!input) return 4;
    org_kde_kwin_fake_input_authenticate(input, "New Desktop CPU regression", "private KWin");
    wl_display_roundtrip(display);
    char kind[16]; double code, value;
    while (scanf("%15s %lf %lf", kind, &code, &value) == 3) {
        if (!strcmp(kind, "motion"))
            org_kde_kwin_fake_input_pointer_motion_absolute(input, wl_fixed_from_double(code), wl_fixed_from_double(value));
        else if (!strcmp(kind, "key") && code >= 0 && code < 256 && (value == 0 || value == 1))
            org_kde_kwin_fake_input_keyboard_key(input, (unsigned)code, (unsigned)value);
        else if (!strcmp(kind, "button") && code == 272 && (value == 0 || value == 1))
            org_kde_kwin_fake_input_button(input, (unsigned)code, (unsigned)value);
        else return 5;
        if (wl_display_roundtrip(display) < 0) return 6;
        puts("ok"); fflush(stdout);
    }
    org_kde_kwin_fake_input_destroy(input);
    wl_display_disconnect(display);
    return 0;
}
