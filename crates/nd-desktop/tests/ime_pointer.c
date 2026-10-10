/* Test-only input goes through a private KWin, never through /dev/uinput. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wayland-client.h>
#include "fake-input.h"

static struct org_kde_kwin_fake_input *input;
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

int main(void) {
    const char *runtime = getenv("XDG_RUNTIME_DIR"), *socket = getenv("WAYLAND_DISPLAY");
    if (!runtime || strcmp(runtime, "/sandbox/runtime") ||
        !socket || strcmp(socket, "nd-test-native") || getenv("WAYLAND_SOCKET"))
        return 2;
    struct wl_display *display = wl_display_connect(NULL);
    if (!display) return 3;
    struct wl_registry *registry = wl_display_get_registry(display);
    wl_registry_add_listener(registry, &listener, NULL);
    if (wl_display_roundtrip(display) < 0 || !input) return 4;
    org_kde_kwin_fake_input_authenticate(input, "New Desktop #66 regression", "private KWin only");
    if (wl_display_roundtrip(display) < 0) return 5;
    char kind[16]; double code, value;
    while (scanf("%15s %lf %lf", kind, &code, &value) == 3) {
        if (!strcmp(kind, "motion"))
            org_kde_kwin_fake_input_pointer_motion_absolute(input, wl_fixed_from_double(code), wl_fixed_from_double(value));
        else if (!strcmp(kind, "button") && code == 272 && (value == 0 || value == 1))
            org_kde_kwin_fake_input_button(input, (uint32_t)code, (uint32_t)value);
        else if (!strcmp(kind, "key") && code >= 0 && code < 256 && (value == 0 || value == 1))
            org_kde_kwin_fake_input_keyboard_key(input, (uint32_t)code, (uint32_t)value);
        else return 6;
        if (wl_display_roundtrip(display) < 0) return 7;
        puts("ok"); fflush(stdout);
    }
    org_kde_kwin_fake_input_destroy(input);
    wl_registry_destroy(registry);
    wl_display_disconnect(display);
    return 0;
}
