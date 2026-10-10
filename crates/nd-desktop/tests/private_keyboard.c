/* Keyboard events enter only the harness's private KWin through EIS. */
#define _POSIX_C_SOURCE 200809L
#include <libei.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <unistd.h>

static struct ei_device *keyboard;
static unsigned char pressed[256];
static volatile sig_atomic_t stopping;
static void stop(int sig) { (void)sig; stopping = 1; }

static void dispatch(struct ei *sender) {
    ei_dispatch(sender);
    struct ei_event *event;
    while ((event = ei_get_event(sender))) {
        if (ei_event_get_type(event) == EI_EVENT_SEAT_ADDED)
            ei_seat_bind_capabilities(ei_event_get_seat(event), EI_DEVICE_CAP_KEYBOARD, NULL);
        if (ei_event_get_type(event) == EI_EVENT_DEVICE_RESUMED) {
            struct ei_device *device = ei_event_get_device(event);
            if (!keyboard && ei_device_has_capability(device, EI_DEVICE_CAP_KEYBOARD)) {
                keyboard = ei_device_ref(device);
                ei_device_start_emulating(keyboard, 1);
            }
        }
        if (ei_event_get_type(event) == EI_EVENT_DISCONNECT) stopping = 1;
        ei_event_unref(event);
    }
}

int main(int argc, char **argv) {
    const char *runtime = getenv("XDG_RUNTIME_DIR"), *socket = getenv("WAYLAND_DISPLAY");
    if (argc != 2 || !runtime || strcmp(runtime, "/sandbox/runtime") ||
        !socket || strcmp(socket, "nd-test-ime") || getenv("WAYLAND_SOCKET")) return 2;
    pid_t parent = getppid();
    if (prctl(PR_SET_PDEATHSIG, SIGTERM) || getppid() != parent) return 3;
    signal(SIGTERM, stop); signal(SIGINT, stop);
    struct ei *sender = ei_new_sender(NULL);
    ei_configure_name(sender, "nd-test-private-keyboard");
    if (ei_setup_backend_fd(sender, atoi(argv[1]))) return 4;
    for (int n = 0; !keyboard && !stopping && n < 100; n++) {
        struct pollfd fd = { ei_get_fd(sender), POLLIN, 0 };
        poll(&fd, 1, 50); dispatch(sender);
    }
    if (!keyboard) return 5;
    puts("ready"); fflush(stdout);
    while (!stopping) {
        struct pollfd fd = { STDIN_FILENO, POLLIN, 0 };
        if (poll(&fd, 1, 100) <= 0) continue;
        unsigned key, value;
        if (scanf("%u %u", &key, &value) != 2) break;
        /* Only the test's text, navigation, paste and Esc keys. */
        if (key >= 256 || value > 1 || !(key == 1 || key == 14 || key == 28 ||
            key == 29 || key == 47 || key == 49 || key == 23 || key == 35 ||
            key == 30 || key == 24 || key == 105 || key == 106)) break;
        pressed[key] = value;
        ei_device_keyboard_key(keyboard, key, value);
        ei_device_frame(keyboard, ei_now(sender));
        ei_dispatch(sender);
        printf("%u %u\n", key, value); fflush(stdout);
    }
    for (unsigned key = 0; key < 256; key++)
        if (pressed[key]) ei_device_keyboard_key(keyboard, key, 0);
    ei_device_frame(keyboard, ei_now(sender));
    ei_dispatch(sender);
    ei_device_stop_emulating(keyboard);
    ei_device_unref(keyboard); ei_unref(sender);
    return 0;
}
