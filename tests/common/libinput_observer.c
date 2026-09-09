/* SPDX-License-Identifier: GPL-3.0-or-later */
/* Event counts for a synthetic touchpad inside the dedicated kernel VM only. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <libinput.h>
#include <poll.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static unsigned motions, buttons, swipe3, swipe4, other_gestures, errors;
static int open_device(const char *path, int flags, void *expected) {
    if (strcmp(path, expected) != 0) return -EACCES;
    int fd = open(path, flags | O_CLOEXEC);
    return fd < 0 ? -errno : fd;
}
static void close_device(int fd, void *unused) { (void)unused; close(fd); }
static void log_event(struct libinput *ctx, enum libinput_log_priority priority,
                      const char *format, va_list arguments) {
    (void)ctx;
    if (priority == LIBINPUT_LOG_PRIORITY_ERROR) {
        errors++;
        fputs("Synthetic VM libinput: ", stderr);
        vfprintf(stderr, format, arguments);
    }
}
static void dispatch(struct libinput *ctx) {
    libinput_dispatch(ctx);
    struct libinput_event *event;
    while ((event = libinput_get_event(ctx))) {
        enum libinput_event_type type = libinput_event_get_type(event);
        if (type == LIBINPUT_EVENT_POINTER_MOTION) motions++;
        if (type == LIBINPUT_EVENT_POINTER_BUTTON) buttons++;
        if (type == LIBINPUT_EVENT_GESTURE_SWIPE_BEGIN) {
            int fingers = libinput_event_gesture_get_finger_count(libinput_event_get_gesture_event(event));
            if (fingers == 3) swipe3++;
            else if (fingers == 4) swipe4++;
            else other_gestures++;
        }
        if (type == LIBINPUT_EVENT_GESTURE_PINCH_BEGIN || type == LIBINPUT_EVENT_GESTURE_HOLD_BEGIN)
            other_gestures++;
        libinput_event_destroy(event);
    }
}
int main(int argc, char **argv) {
    char boot[4096] = {0};
    FILE *cmdline = fopen("/proc/cmdline", "r");
    if (!cmdline || !fgets(boot, sizeof(boot), cmdline) ||
        !strstr(boot, "niri-bridge-kernel-test=1") || argc != 2) return 2;
    fclose(cmdline);
    const struct libinput_interface interface = { open_device, close_device };
    struct libinput *ctx = libinput_path_create_context(&interface, argv[1]);
    if (!ctx) return 3;
    libinput_log_set_handler(ctx, log_event);
    struct libinput_device *device = libinput_path_add_device(ctx, argv[1]);
    if (!device) return 4;
    if (!libinput_device_config_tap_get_finger_count(device)) return 5;
    libinput_device_config_tap_set_enabled(device, LIBINPUT_CONFIG_TAP_ENABLED);
    dispatch(ctx);
    puts("READY"); fflush(stdout);
    struct pollfd fds[] = {{libinput_get_fd(ctx), POLLIN, 0}, {STDIN_FILENO, POLLIN, 0}};
    for (;;) {
        if (poll(fds, 2, 500) < 0 && errno != EINTR) return 6;
        dispatch(ctx);
        if (fds[1].revents & POLLHUP) break;
        if (fds[1].revents & POLLIN) {
            char command[32];
            if (!fgets(command, sizeof(command), stdin) || !strcmp(command, "quit\n")) break;
            if (!strcmp(command, "snapshot\n")) {
                printf("%u %u %u %u %u %u\n", motions, buttons, swipe3, swipe4, other_gestures, errors);
                fflush(stdout);
            }
        }
    }
    libinput_path_remove_device(device);
    libinput_unref(ctx);
    return 0;
}
