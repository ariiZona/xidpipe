// SPDX-License-Identifier: GPL-3.0-or-later
// On-screen menu: hold Back for 10 seconds on any controller to open it. D-pad moves, A chooses,
// B closes. While it's open the PC gets a neutral controller, so nothing reaches the game.

#include "xidpipe.h"
#include <hal/debug.h>
#include <windows.h>
#include <xboxkrnl/xboxkrnl.h>

#define HOLD_MS 10000
#define BTN_UP 0x01
#define BTN_DOWN 0x02
#define BTN_BACK 0x20

enum { ITEM_FTP, ITEM_REBOOT, ITEM_SHUTDOWN, ITEM_CLOSE, ITEM_COUNT };

volatile int menu_open;
static int selected;
static DWORD back_since;
static uint8_t prev_digital;
static int prev_a, prev_b;

static void draw(void)
{
    static const char *const labels[ITEM_COUNT] = {NULL, "Reboot the Xbox", "Shut down the Xbox", "Close this menu"};
    debugClearScreen();
    debugPrint("xidpipe v" XIDPIPE_VERSION " menu\n\n");
    for (int i = 0; i < ITEM_COUNT; i++) {
        const char *label = labels[i];
        if (i == ITEM_FTP) {
            label = ftp_is_enabled() ? "FTP server: on   (A turns it off)" : "FTP server: off  (A turns it on)";
        }
        debugPrint("%s %s\n", i == selected ? ">" : " ", label);
    }
    debugPrint("\nD-pad: move   A: choose   B: close\n");
    debugPrint("The PC gets a neutral controller while this menu is open.\n");
}

static void close_menu(void)
{
    menu_open = 0;
    screen_resume(); // brings the log back
}

int menu_update(uint8_t digital, int a, int b, uint32_t now)
{
    if (!menu_open) {
        if (!(digital & BTN_BACK)) {
            back_since = 0;
        } else if (back_since == 0) {
            back_since = now ? now : 1;
        } else if (now - back_since >= HOLD_MS) {
            back_since = 0;
            selected = 0;
            prev_digital = digital; // what's already held doesn't count as a press
            prev_a = a;
            prev_b = b;
            screen_pause();
            menu_open = 1;
            draw();
        }
        return menu_open;
    }

    int up = (digital & BTN_UP) && !(prev_digital & BTN_UP);
    int down = (digital & BTN_DOWN) && !(prev_digital & BTN_DOWN);
    int pick = a && !prev_a;
    int back = b && !prev_b;
    prev_digital = digital;
    prev_a = a;
    prev_b = b;

    if (back) {
        close_menu();
    } else if (up || down) {
        selected = (selected + (down ? 1 : ITEM_COUNT - 1)) % ITEM_COUNT;
        draw();
    } else if (pick) {
        switch (selected) {
        case ITEM_FTP:
            ftp_set_enabled(!ftp_is_enabled());
            draw();
            break;
        case ITEM_REBOOT:
            debugClearScreen();
            debugPrint("Rebooting...\n");
            HalReturnToFirmware(HalRebootRoutine);
            break;
        case ITEM_SHUTDOWN:
            debugClearScreen();
            debugPrint("Shutting down...\n");
            HalInitiateShutdown();
            for (;;) {
                Sleep(1000);
            }
        default:
            close_menu();
            break;
        }
    }
    return menu_open;
}
