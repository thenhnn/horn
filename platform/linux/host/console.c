// SPDX-License-Identifier: GPL-2.0-only
// One console subscription covers RX and blocked TX. Buffer RX while waiting
// for writable space so readable input cannot starve a blocked console write.
#define CONSOLE_RX_BYTES 4096
static unsigned char console_rx[CONSOLE_RX_BYTES];
static unsigned console_rx_head, console_rx_count;
static bool console_input_enabled, console_closed;

static void console_arm(bool writable)
{
    if (console_closed) {
        return;
    }
    horn_ConsoleinterfaceWatchArgs_t args = {0};
    if (console_input_enabled && console_rx_count < CONSOLE_RX_BYTES) {
        args.interests |= HORN_CONSOLE_EVENTS_READABLE;
    }
    if (writable) {
        args.interests |= HORN_CONSOLE_EVENTS_WRITABLE;
    }
    args.notification.present = args.interests != 0;
    args.notification.value = cap(initial.console_signal);
    horn_console_interface_watch_result_t result;
    CHECK(horn_console_interface_watch(&transport, 0, cap(initial.console), args.notification,
                                       args.interests, 0, &result),
          result);
    if (result.value.ok.events & HORN_CONSOLE_EVENTS_CLOSED) {
        console_closed = true;
        pending |= CONSOLE_EVENT;
    }
}

static void console_receive(void)
{
    while (console_input_enabled && !console_closed && console_rx_count < CONSOLE_RX_BYTES) {
        unsigned maximum = CONSOLE_RX_BYTES - console_rx_count;
        if (maximum > 256) {
            maximum = 256;
        }
        horn_console_interface_read_result_t result;
        if (horn_console_interface_read(&transport, 0, cap(initial.console), maximum, &result) !=
            HORN_OK) {
            die();
        }
        if (result.tag != HORN_RESULT_OK) {
            if (result.value.err == HORN_CONSOLE_ERROR_WOULD_BLOCK) {
                break;
            }
            if (result.value.err == HORN_CONSOLE_ERROR_CLOSED) {
                console_closed = true;
                pending |= CONSOLE_EVENT;
                break;
            }
            die();
        }
        unsigned length = result.value.ok.len;
        if (!length || length > maximum) {
            die();
        }
        for (unsigned i = 0; i < length; i++) {
            console_rx[(console_rx_head + console_rx_count++) % CONSOLE_RX_BYTES] =
                result.value.ok.data[i];
        }
        pending |= CONSOLE_EVENT;
    }
}

void horn_console_enable(int enabled)
{
    console_input_enabled = enabled != 0;
    console_arm(false);
}

long horn_console_read(char *data, unsigned long maximum)
{
    console_receive();
    unsigned long length = 0;
    while (length < maximum && console_rx_count) {
        data[length++] = console_rx[console_rx_head];
        console_rx_head = (console_rx_head + 1) % CONSOLE_RX_BYTES;
        console_rx_count--;
    }
    console_arm(false);
    return length ? (long)length : console_closed ? -32 : 0; // -EPIPE: HVC hangup.
}

int horn_console_pending(void)
{
    seL4_Word badge = 0;
    seL4_Poll(initial.events, &badge);
    pending |= badge;
    bool ready = console_input_enabled && ((pending & CONSOLE_EVENT) || console_rx_count);
    pending &= ~CONSOLE_EVENT;
    return ready;
}

void horn_console_write(const char *data, unsigned long length)
{
    while (length && !console_closed) {
        horn_list_256_u8_t bytes = {0};
        bytes.len = length < 256 ? length : 256;
        memcpy(bytes.data, data, bytes.len);
        horn_console_interface_write_result_t result;
        if (horn_console_interface_write(&transport, 0, cap(initial.console), bytes, &result) !=
            HORN_OK) {
            die();
        }
        if (result.tag != HORN_RESULT_OK) {
            if (result.value.err == HORN_CONSOLE_ERROR_CLOSED) {
                console_closed = true;
                pending |= CONSOLE_EVENT;
                return;
            }
            if (result.value.err != HORN_CONSOLE_ERROR_WOULD_BLOCK) {
                die();
            }
            console_receive();
            pending &= ~CONSOLE_EVENT;
            console_arm(true);
            if (!(pending & CONSOLE_EVENT)) {
                seL4_Word badge;
                seL4_Wait(initial.events, &badge);
                pending |= badge;
            }
            // RX retained above will wake HVC after the write completes.
            continue;
        }
        if (!result.value.ok || result.value.ok > bytes.len) {
            die();
        }
        data += result.value.ok;
        length -= result.value.ok;
    }
    console_arm(false);
    if (console_rx_count) {
        pending |= CONSOLE_EVENT;
    }
}
