// SPDX-License-Identifier: GPL-3.0-or-later
// xidpipe, Xbox side: forwards raw XID controller reports to the PC app over UDP.
//
// Latency path: USB interrupt -> DPC copies the report and signals `wake` -> main thread
// sends it straight away. Packets from the PC (hello, rumble) arrive in an lwIP callback
// that also signals `wake`, so rumble reaches the USB bus without waiting for a timer.
// Reliability: every packet carries the full controller state, and the state is resent
// every RESEND_MS even when nothing changed, so a lost packet is fixed by the next one.
// Rumble comes back from the PC the same way (full state, resent), and stops by itself if
// the PC goes quiet, so a lost packet or a crashed PC can't leave the motors running.

#include "xidpipe.h"
#include <hal/debug.h>
#include <hal/video.h>
#include <hal/xbox.h>
#include <nxdk/mount.h>
#include <nxdk/path.h>
#include <windows.h>
#include <xboxkrnl/xboxkrnl.h>
#include <nxdk/net.h>
#include <lwip/netif.h>
#include <lwip/pbuf.h>
#include <lwip/tcpip.h>
#include <lwip/udp.h>
#include <lwip/dns.h>
#include <usbh_lib.h>
#include <usb.h>
#include <hub.h>
#include <stdarg.h>
#include <stdio.h>
#include <string.h>

#define XP_PORT 47100 // keep in sync with pc/src/engine.rs
#define XP_VERSION 1
#define MSG_STATE 1
#define MSG_UNPLUGGED 2
#define MSG_PONG 3
#define MSG_INFO 4
#define MSG_HELLO 0x10
#define MSG_RUMBLE 0x11
#define HEADER 14
#define REPORT_MAX 32
#define MAX_PADS 8
#define RESEND_MS 50
#define INFO_MS 1000
#define PEER_TIMEOUT_MS 3000
#define RETRY_MS 500
#define RUMBLE_TIMEOUT_MS 1000 // the PC resends every 250 ms
#define RUMBLE_RETRY_MS 250    // a failed rumble write is sent again after this
#define AUTO_MAX_INTERVAL_MS 8 // "auto" polling never goes below 125 Hz
#define XID_CLASS 0x58

typedef struct {
    IFACE_T *iface;           // NULL: slot is free
    UTR_T *utr;
    uint8_t id;               // 1-4: front port number, 5+: port unknown
    uint16_t vid, pid;
    uint8_t xid_type, xid_sub; // 0: the pad has no XID descriptor (some 3rd-party pads)
    uint8_t dev_interval;     // bInterval the controller asks for, in ms
    uint8_t interval;         // interval we poll it at, in ms
    volatile uint32_t gen;    // bumped by the DPC for every report
    volatile uint8_t failed;  // reading stopped (USB error or interval change); main loop re-arms
    volatile uint8_t len;
    volatile uint16_t errors;
    volatile ULONGLONG last_report_tc; // performance counter at the last report (DPC)
    volatile ULONGLONG worst_gap_tc;   // longest gap between reports since the last info
    uint8_t report[REPORT_MAX];
    uint32_t sent_gen, info_gen;
    DWORD sent_at, failed_at, info_at;
    UTR_T *out_utr;           // rumble writes; NULL if the pad has no OUT endpoint
    volatile uint8_t out_busy;
    volatile uint8_t out_failed; // the last rumble write failed (set by the DPC or the main loop)
    DWORD out_failed_at;
    uint16_t rumble_sent[2];
} pad_t;

extern struct netif *g_pnetif;

static pad_t pads[MAX_PADS];
static uint32_t seqs[16]; // per pad id; never reset, so the PC can drop stale packets
static KEVENT wake;
static CRITICAL_SECTION screen_lock;
#define LOG_LINES 24
static char log_lines[LOG_LINES][160];
static int log_next, screen_paused;
static char self_path[MAX_PATH]; // "Q:\default.xbe" (Q: is the XBE's own folder)
static char self_real[MAX_PATH]; // the same file on its real drive, e.g. "E:\Apps\xidpipe\default.xbe"
static ULONGLONG perf_hz;
static uint8_t poll_ms;   // applied polling setting from the PC: 0 = auto, else 1/2/4/8 ms

// Shared with the lwIP callback; only touched while holding the lwIP core lock.
static struct udp_pcb *pcb;
static ip_addr_t peer_ip;
static u16_t peer_port;
static int have_peer, peer_changed;
static DWORD peer_seen;
static uint8_t want_poll_ms;
static uint16_t want_rumble[16][2]; // by pad id
static DWORD rumble_seen[16];

void say(const char *fmt, ...)
{
    char b[512];
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(b, sizeof(b) - 1, fmt, ap);
    va_end(ap);
    if (n < 0) {
        return;
    }
    if (n > (int)sizeof(b) - 2) {
        n = (int)sizeof(b) - 2;
    }
    if (n == 0 || b[n - 1] != '\n') {
        b[n++] = '\n';
        b[n] = 0;
    }
    EnterCriticalSection(&screen_lock);
    snprintf(log_lines[log_next], sizeof(log_lines[0]), "%s", b);
    log_next = (log_next + 1) % LOG_LINES;
    if (!screen_paused) {
        debugPrint("%s", b);
    }
    LeaveCriticalSection(&screen_lock);
}

void screen_pause(void)
{
    EnterCriticalSection(&screen_lock);
    screen_paused = 1;
    LeaveCriticalSection(&screen_lock);
}

void screen_resume(void)
{
    EnterCriticalSection(&screen_lock);
    screen_paused = 0;
    debugClearScreen();
    for (int i = 0; i < LOG_LINES; i++) {
        const char *line = log_lines[(log_next + i) % LOG_LINES];
        if (line[0]) {
            debugPrint("%s", line);
        }
    }
    LeaveCriticalSection(&screen_lock);
}

void set_marker(const char *name, int present)
{
    char path[MAX_PATH];
    snprintf(path, sizeof(path), "Q:\\%s", name);
    if (!present) {
        DeleteFileA(path);
        return;
    }
    HANDLE f = CreateFileA(path, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (f != INVALID_HANDLE_VALUE) {
        CloseHandle(f);
    }
}

const char *self_xbe_path(void)
{
    return self_path;
}

const char *self_xbe_name(void)
{
    return self_path + 3; // after "Q:\"
}

const char *self_xbe_real_path(void)
{
    return self_real;
}

// The drive letters Xbox dashboards use (same layout as xemu-dashboard). F and G only exist on
// big, extra-partitioned drives; D is the DVD drive.
static const struct {
    char letter;
    const char *device;
} drives[] = {
    {'C', "\\Device\\Harddisk0\\Partition2"}, {'E', "\\Device\\Harddisk0\\Partition1"},
    {'F', "\\Device\\Harddisk0\\Partition6"}, {'G', "\\Device\\Harddisk0\\Partition7"},
    {'X', "\\Device\\Harddisk0\\Partition3"}, {'Y', "\\Device\\Harddisk0\\Partition4"},
    {'Z', "\\Device\\Harddisk0\\Partition5"}, {'D', "\\Device\\CdRom0"},
};

// Mount the drives for the FTP server, and xidpipe's own folder as Q:. nxdk mounts the XBE's
// folder as D:, but dashboards use D: for the DVD drive, so it's moved.
static void mount_drives(void)
{
    char nt[MAX_PATH];
    nxGetCurrentXbeNtPath(nt);
    char *slash = strrchr(nt, '\\');
    snprintf(self_path, sizeof(self_path), "Q:\\%s", slash ? slash + 1 : "default.xbe");
    if (slash) {
        slash[1] = 0;
        nxMountDrive('Q', nt);
    }
    nxUnmountDrive('D');
    for (size_t i = 0; i < sizeof(drives) / sizeof(drives[0]); i++) {
        char path[MAX_PATH];
        snprintf(path, sizeof(path), "%s%s", drives[i].device, drives[i].letter == 'D' ? "" : "\\");
        nxMountDrive(drives[i].letter, path);
    }

    // Where the running XBE is on those drives, so the FTP server can protect it.
    nxGetCurrentXbeNtPath(nt);
    for (size_t i = 0; i < sizeof(drives) / sizeof(drives[0]); i++) {
        size_t n = strlen(drives[i].device);
        if (_strnicmp(nt, drives[i].device, n) == 0 && nt[n] == '\\') {
            snprintf(self_real, sizeof(self_real), "%c:%s", drives[i].letter, nt + n);
        }
    }
}

int marker_exists(const char *name)
{
    char path[MAX_PATH];
    snprintf(path, sizeof(path), "Q:\\%s", name);
    return GetFileAttributesA(path) != INVALID_FILE_ATTRIBUTES;
}

int looks_like_xbe(const uint8_t *data, uint32_t len)
{
    return data != NULL && len >= 0x1000 && memcmp(data, "XBEH", 4) == 0;
}

int install_xbe(const char *new_path)
{
    char old[MAX_PATH + 8];
    snprintf(old, sizeof(old), "%s.old", self_path);
    DeleteFileA(old);
    if (!MoveFileA(self_path, old)) {
        return 0;
    }
    if (!MoveFileA(new_path, self_path)) {
        MoveFileA(old, self_path); // put the working one back
        return 0;
    }
    DeleteFileA(old);
    return 1;
}

void relaunch_self(void)
{
    char nt[MAX_PATH];
    nxGetCurrentXbeNtPath(nt);
    XLaunchXBE(nt);
    for (;;) {
        Sleep(1000);
    }
}

// Caller holds the lwIP core lock.
static void send_raw(const ip_addr_t *to, u16_t port, const void *data, u16_t len)
{
    struct pbuf *q = pbuf_alloc(PBUF_TRANSPORT, len, PBUF_RAM);
    if (q == NULL) {
        return;
    }
    memcpy(q->payload, data, len);
    udp_sendto(pcb, q, to, port);
    pbuf_free(q);
}

static void send_peer(const void *data, u16_t len)
{
    LOCK_TCPIP_CORE();
    if (have_peer) {
        send_raw(&peer_ip, peer_port, data, len);
    }
    UNLOCK_TCPIP_CORE();
}

// lwIP callback (tcpip thread, core lock held): hello and rumble from the PC.
static void on_udp(void *arg, struct udp_pcb *upcb, struct pbuf *p, const ip_addr_t *addr, u16_t port)
{
    (void)arg;
    (void)upcb;
    uint8_t b[16];
    u16_t n = pbuf_copy_partial(p, b, sizeof(b), 0);
    pbuf_free(p);
    if (n < 4 || b[0] != 'X' || b[1] != 'P' || b[2] != XP_VERSION) {
        return;
    }
    DWORD now = GetTickCount();

    if (b[3] == MSG_HELLO) {
        if (!have_peer || !ip_addr_eq(&peer_ip, addr) || peer_port != port) {
            peer_changed = 1;
        }
        ip_addr_copy(peer_ip, *addr);
        peer_port = port;
        have_peer = 1;
        peer_seen = now;
        if (n >= 5) {
            want_poll_ms = b[4];
        }
        if (n >= 10) {
            // Echo the PC's timestamp right away so it can measure the round trip.
            uint8_t pong[10] = {'X', 'P', XP_VERSION, MSG_PONG, 0, 0, b[6], b[7], b[8], b[9]};
            send_raw(addr, port, pong, sizeof(pong));
        }
    } else if (b[3] == MSG_RUMBLE && n >= 9 && have_peer && ip_addr_eq(&peer_ip, addr) && peer_port == port) {
        // Only the PC we stream to may drive the motors.
        uint8_t id = b[4] & 15;
        want_rumble[id][0] = b[5] | b[6] << 8;
        want_rumble[id][1] = b[7] | b[8] << 8;
        rumble_seen[id] = now;
    } else {
        return;
    }
    KeSetEvent(&wake, 1, FALSE);
}

static void send_pad(pad_t *p, uint8_t type)
{
    uint8_t b[HEADER + REPORT_MAX];
    uint8_t len = 0;

    if (type == MSG_STATE) {
        // Single CPU: at DISPATCH_LEVEL the USB DPC can't run, so the copy is consistent.
        KIRQL irql = KeRaiseIrqlToDpcLevel();
        len = p->len;
        memcpy(b + HEADER, p->report, len);
        p->sent_gen = p->gen;
        KfLowerIrql(irql);
        if (menu_open) {
            // The menu is using the controller: the PC gets one with nothing pressed.
            len = 20;
            memset(b + HEADER, 0, len);
            b[HEADER + 1] = 20;
        }
    }
    p->sent_at = GetTickCount();

    uint32_t seq = ++seqs[p->id & 15];
    b[0] = 'X';
    b[1] = 'P';
    b[2] = XP_VERSION;
    b[3] = type;
    b[4] = p->id;
    b[5] = len;
    b[6] = p->vid;
    b[7] = p->vid >> 8;
    b[8] = p->pid;
    b[9] = p->pid >> 8;
    b[10] = seq;
    b[11] = seq >> 8;
    b[12] = seq >> 16;
    b[13] = seq >> 24;
    send_peer(b, HEADER + len);
}

// Once a second: what the PC's debug view shows about this pad.
static void send_info(pad_t *p, DWORD now)
{
    uint32_t elapsed = now - p->info_at;
    uint32_t rate = elapsed ? (p->gen - p->info_gen) * 1000 / elapsed : 0;
    uint16_t errors = p->errors;
    KIRQL irql = KeRaiseIrqlToDpcLevel();
    ULONGLONG worst = p->worst_gap_tc;
    p->worst_gap_tc = 0;
    KfLowerIrql(irql);
    // Longest gap between two controller reports, in 0.1 ms, measured before the network.
    ULONGLONG gap = perf_hz ? worst * 10000 / perf_hz : 0;
    uint16_t gap16 = gap > 0xFFFF ? 0xFFFF : (uint16_t)gap;
    uint8_t b[16] = {'X', 'P', XP_VERSION, MSG_INFO, p->id, p->dev_interval, p->interval, p->xid_type,
                     p->xid_sub, p->out_utr != NULL, rate, rate >> 8, errors, errors >> 8, gap16, gap16 >> 8};
    p->info_gen = p->gen;
    p->info_at = now;
    send_peer(b, sizeof(b));
}

// Runs in the USB DPC. Copy the report, re-arm the read, wake the main thread.
static void on_report(UTR_T *utr)
{
    pad_t *p = (pad_t *)utr->context;

    if (p->iface == NULL || p->failed) {
        return; // unplugged, or the main loop is re-arming this pad
    }
    if (utr->status < 0) {
        p->failed = 1;
        p->errors++;
        KeSetEvent(&wake, 1, FALSE);
        return;
    }

    uint32_t n = utr->xfer_len;
    if (n > 0) {
        if (n > REPORT_MAX) {
            n = REPORT_MAX;
        }
        memcpy(p->report, utr->buff, n);
        p->len = n;
        p->gen++;
        ULONGLONG tc = KeQueryPerformanceCounter();
        if (p->last_report_tc && tc - p->last_report_tc > p->worst_gap_tc) {
            p->worst_gap_tc = tc - p->last_report_tc;
        }
        p->last_report_tc = tc;
    }

    utr->xfer_len = 0;
    utr->bIsTransferDone = 0;
    if (usbh_int_xfer(utr) != USBH_OK) {
        p->failed = 1;
        p->errors++;
    }
    KeSetEvent(&wake, 1, FALSE);
}

// Runs in the USB DPC when a rumble write finished or failed (update_rumble retries failures).
static void on_rumble_done(UTR_T *utr)
{
    pad_t *p = (pad_t *)utr->context;
    if (utr->status < 0) {
        p->out_failed = 1;
    }
    p->out_busy = 0;
    KeSetEvent(&wake, 1, FALSE);
}

static void update_rumble(pad_t *p, const uint16_t want[2], DWORD now)
{
    if (p->out_utr == NULL || p->out_busy) {
        return;
    }
    if (p->out_failed && p->out_failed_at == 0) {
        p->out_failed_at = now ? now : 1; // failed in the DPC
    }
    // A failed write is resent, so a lost "motors off" can't leave them running.
    int retry = p->out_failed && now - p->out_failed_at >= RUMBLE_RETRY_MS;
    if (!retry && want[0] == p->rumble_sent[0] && want[1] == p->rumble_sent[1]) {
        return;
    }
    // XID rumble report: id 0, length 6, left u16, right u16
    uint8_t *b = p->out_utr->buff;
    b[0] = 0;
    b[1] = 6;
    b[2] = want[0];
    b[3] = want[0] >> 8;
    b[4] = want[1];
    b[5] = want[1] >> 8;
    p->out_utr->data_len = 6;
    p->out_utr->xfer_len = 0;
    p->out_utr->bIsTransferDone = 0;
    // No transfer is in flight (out_busy is 0), so the DPC can't touch out_failed here.
    p->rumble_sent[0] = want[0];
    p->rumble_sent[1] = want[1];
    p->out_failed = 0;
    p->out_failed_at = 0;
    p->out_busy = 1;
    if (usbh_int_xfer(p->out_utr) != USBH_OK) {
        p->out_busy = 0;
        p->out_failed = 1; // retried every RUMBLE_RETRY_MS, not every loop
        p->out_failed_at = now ? now : 1;
    }
}

// Polling interval for a pad: the PC's choice, or the controller's own but at least 125 Hz.
static uint8_t interval_for(const pad_t *p)
{
    if (poll_ms) {
        return poll_ms;
    }
    return p->dev_interval && p->dev_interval < AUTO_MAX_INTERVAL_MS ? p->dev_interval : AUTO_MAX_INTERVAL_MS;
}

// Change a live pad's polling interval. OHCI fixes the interval when the endpoint is first
// queued, so stop the endpoint and let retry_read re-arm it (that also resets the data toggle).
static void set_interval(pad_t *p, uint8_t ms, DWORD now)
{
    KIRQL irql = KeRaiseIrqlToDpcLevel();
    p->failed = 1; // stops on_report from re-queueing
    KfLowerIrql(irql);
    p->iface->udev->hc_driver->quit_xfer(NULL, p->utr->ep);
    p->utr->ep->bInterval = ms;
    p->interval = ms;
    p->failed_at = now ? now : 1;
}

// Front panel port (1-4) of a device, 0 if unknown. Same mapping as nxdk's SDL driver.
static uint8_t front_port(UDEV_T *udev)
{
    static const uint8_t root_to_front[5] = {0, 3, 4, 1, 2};
    int internal_hub = XboxHardwareInfo.Flags & XBOX_HW_FLAG_INTERNAL_USB_HUB;

    while (udev != NULL) {
        UDEV_T *parent = udev->parent ? udev->parent->iface->udev : NULL;
        if (internal_hub ? (parent != NULL && parent->parent == NULL) : (udev->parent == NULL)) {
            return udev->port_num <= 4 ? root_to_front[udev->port_num] : 0;
        }
        udev = parent;
    }
    return 0;
}

static int xp_probe(IFACE_T *iface)
{
    UDEV_T *udev = iface->udev;

    if (iface->aif->ifd->bInterfaceClass != XID_CLASS) {
        return USBH_ERR_NOT_MATCHED;
    }

    // Official pads answer the XID descriptor request; some 3rd-party pads don't, and nxdk's
    // own xid driver rejects those. Here a missing descriptor just means "assume a gamepad".
    uint8_t type = 0, sub = 0;
    uint8_t *desc = usbh_alloc_mem(16);
    if (desc != NULL) {
        uint32_t got = 0;
        if (usbh_ctrl_xfer(udev, 0xC1, 0x06, 0x4200, iface->if_num, 16, desc, &got, 100) == USBH_OK && got >= 6) {
            type = desc[4];
            sub = desc[5];
        }
        usbh_free_mem(desc, 16);
    }
    if (type == 0x03 || type == 0x80) {
        return USBH_ERR_NOT_MATCHED; // DVD remote, Steel Battalion: not gamepads
    }

    EP_INFO_T *ep = usbh_iface_find_ep(iface, 0, EP_ADDR_DIR_IN | EP_ATTR_TT_INT);
    if (ep == NULL) {
        return USBH_ERR_NOT_MATCHED;
    }

    pad_t *p = NULL;
    for (int i = 0; i < MAX_PADS; i++) {
        if (pads[i].iface == NULL) {
            p = &pads[i];
            break;
        }
    }
    if (p == NULL) {
        return USBH_ERR_MEMORY_OUT;
    }

    UTR_T *utr = alloc_utr(udev);
    if (utr == NULL) {
        return USBH_ERR_MEMORY_OUT;
    }
    utr->buff = usbh_alloc_mem(ep->wMaxPacketSize);
    if (utr->buff == NULL) {
        free_utr(utr);
        return USBH_ERR_MEMORY_OUT;
    }

    memset(p, 0, sizeof(*p));
    uint8_t port = front_port(udev);
    p->id = port ? port : 5 + (p - pads);
    p->vid = udev->descriptor.idVendor;
    p->pid = udev->descriptor.idProduct;
    p->xid_type = type;
    p->xid_sub = sub;
    p->dev_interval = ep->bInterval;
    p->interval = interval_for(p);
    ep->bInterval = p->interval; // read by OHCI when the endpoint is first queued below
    p->info_at = GetTickCount();
    p->utr = utr;
    utr->context = p;
    utr->ep = ep;
    utr->data_len = ep->wMaxPacketSize;
    utr->xfer_len = 0;
    utr->func = on_report;
    p->iface = iface;
    iface->context = p;

    if (usbh_int_xfer(utr) != USBH_OK) {
        p->failed = 1; // the main loop retries
    }

    // Rumble is optional: some 3rd-party pads have no motors or no OUT endpoint.
    EP_INFO_T *out_ep = usbh_iface_find_ep(iface, 0, EP_ADDR_DIR_OUT | EP_ATTR_TT_INT);
    UTR_T *out = out_ep ? alloc_utr(udev) : NULL;
    if (out != NULL) {
        out->buff = usbh_alloc_mem(out_ep->wMaxPacketSize);
        if (out->buff == NULL || out_ep->wMaxPacketSize < 6) {
            if (out->buff != NULL) {
                usbh_free_mem(out->buff, out_ep->wMaxPacketSize);
            }
            free_utr(out);
            out = NULL;
        } else {
            out->context = p;
            out->ep = out_ep;
            out->func = on_rumble_done;
            out_ep->bInterval = 1; // rumble goes out on the next 1 ms frame
        }
    }
    p->out_utr = out;
    say("Pad %u connected (%04x:%04x, XID %02x/%02x, polled every %u ms)\n", p->id, p->vid, p->pid, type, sub,
               p->interval);
    return USBH_OK;
}

static void xp_disconnect(IFACE_T *iface)
{
    pad_t *p = (pad_t *)iface->context;
    if (p == NULL) {
        return;
    }

    // Stop the hardware and the DPC callbacks before the buffers go away.
    for (int i = 0; i < iface->aif->ifd->bNumEndpoints; i++) {
        iface->udev->hc_driver->quit_xfer(NULL, &(iface->aif->ep[i]));
    }
    usbh_quit_utr(p->utr);
    KIRQL irql = KeRaiseIrqlToDpcLevel();
    p->iface = NULL;
    KfLowerIrql(irql);
    usbh_free_mem(p->utr->buff, p->utr->ep->wMaxPacketSize);
    free_utr(p->utr);
    if (p->out_utr != NULL) {
        usbh_quit_utr(p->out_utr);
        usbh_free_mem(p->out_utr->buff, p->out_utr->ep->wMaxPacketSize);
        free_utr(p->out_utr);
        p->out_utr = NULL;
    }
    iface->context = NULL;

    send_pad(p, MSG_UNPLUGGED);
    say("Pad %u disconnected\n", p->id);
}

// Re-arm a read that stopped (USB error, 3rd-party glitch, or an interval change).
static void retry_read(pad_t *p, DWORD now)
{
    if (p->failed_at == 0) {
        p->failed_at = now ? now : 1;
        return;
    }
    if (now - p->failed_at < RETRY_MS) {
        return;
    }
    usbh_clear_halt(p->iface->udev, p->utr->ep->bEndpointAddress);
    p->failed_at = 0;
    p->utr->xfer_len = 0;
    p->utr->bIsTransferDone = 0;
    KIRQL irql = KeRaiseIrqlToDpcLevel();
    p->failed = 0;
    KfLowerIrql(irql);
    if (usbh_int_xfer(p->utr) != USBH_OK) {
        p->failed = 1;
    }
}

int main(void)
{
    static UDEV_DRV_T xp_driver = {xp_probe, xp_disconnect, NULL, NULL};

    XVideoSetMode(640, 480, 32, REFRESH_DEFAULT);
    InitializeCriticalSection(&screen_lock);
    mount_drives();
    say("xidpipe v" XIDPIPE_VERSION "\n\nStarting network...\n");

    if (nxNetInit(NULL) != 0) {
        say("Network failed to start. Check the cable and DHCP, then restart xidpipe.\n");
        for (;;) {
            Sleep(1000);
        }
    }

    KeInitializeEvent(&wake, SynchronizationEvent, FALSE);
    perf_hz = KeQueryPerformanceFrequency();

    LOCK_TCPIP_CORE();
    pcb = udp_new();
    err_t bound = pcb ? udp_bind(pcb, IP_ANY_TYPE, XP_PORT) : ERR_MEM;
    if (bound == ERR_OK) {
        udp_recv(pcb, on_udp, NULL);
    }
    UNLOCK_TCPIP_CORE();
    if (bound != ERR_OK) {
        say("Could not open UDP port %d.\n", XP_PORT);
        for (;;) {
            Sleep(1000);
        }
    }
    say("Xbox IP: %s  (UDP %d)\n", ip4addr_ntoa(netif_ip4_addr(g_pnetif)), XP_PORT);
    {
        // The update check needs DNS. A manual IP set in the dashboard without a DNS server leaves
        // none: fall back to the router, which answers DNS on most home networks.
        char dns_text[IPADDR_STRLEN_MAX];
        LOCK_TCPIP_CORE();
        const ip_addr_t *dns = dns_getserver(0);
        int had_dns = dns != NULL && !ip_addr_isany(dns);
        const ip4_addr_t *gw = netif_ip4_gw(g_pnetif);
        if (!had_dns && gw != NULL && !ip4_addr_isany(gw)) {
            ip_addr_t router;
            ip_addr_copy_from_ip4(router, *gw);
            dns_setserver(0, &router);
        }
        ipaddr_ntoa_r(dns_getserver(0), dns_text, sizeof(dns_text));
        UNLOCK_TCPIP_CORE();
        if (had_dns) {
            say("DNS: %s", dns_text);
        } else {
            say("No DNS server set on the Xbox; using the router (%s)", dns_text);
        }
    }
    say("Run xidpipe on the PC. If it can't find the Xbox, enter this IP in xidpipe.");
    say("Hold Back for 10 seconds for the menu: FTP on/off, reboot, shut down.\n");
    ftp_start();
    update_start();

    usbh_core_init();
    usbh_register_driver(&xp_driver);

    DWORD a_held_since = 0;
    for (;;) {
        // Woken at once by a USB report or a PC packet; the timeout paces hotplug and resends.
        LARGE_INTEGER timeout;
        timeout.QuadPart = -4 * 10000LL; // 4 ms
        KeWaitForSingleObject(&wake, Executive, KernelMode, FALSE, &timeout);

        usbh_pooling_hubs();

        // Snapshot what the network callback shared. `now` is read under the lock so it is
        // never older than a timestamp the callback wrote.
        uint16_t rumble[16][2];
        LOCK_TCPIP_CORE();
        DWORD now = GetTickCount();
        int gone = have_peer && now - peer_seen > PEER_TIMEOUT_MS;
        if (gone) {
            have_peer = 0;
        }
        int connected = peer_changed ? 1 : 0;
        peer_changed = 0;
        uint8_t want_poll = want_poll_ms;
        for (int id = 0; id < 16; id++) {
            int live = have_peer && now - rumble_seen[id] <= RUMBLE_TIMEOUT_MS;
            rumble[id][0] = live ? want_rumble[id][0] : 0;
            rumble[id][1] = live ? want_rumble[id][1] : 0;
        }
        char peer_text[IPADDR_STRLEN_MAX];
        ipaddr_ntoa_r(&peer_ip, peer_text, sizeof(peer_text));
        UNLOCK_TCPIP_CORE();

        if (connected) {
            say("PC connected: %s\n", peer_text);
        }
        if (gone) {
            say("PC gone, waiting for it again\n");
        }
        if (want_poll != poll_ms && (want_poll == 0 || want_poll == 1 || want_poll == 2 || want_poll == 4 || want_poll == 8)) {
            poll_ms = want_poll;
            say("Polling: %s%u ms\n", poll_ms ? "every " : "auto, at most ", poll_ms ? poll_ms : AUTO_MAX_INTERVAL_MS);
            for (int i = 0; i < MAX_PADS; i++) {
                if (pads[i].iface != NULL && interval_for(&pads[i]) != pads[i].interval) {
                    set_interval(&pads[i], interval_for(&pads[i]), now);
                }
            }
        }

        for (int i = 0; i < MAX_PADS; i++) {
            pad_t *p = &pads[i];
            if (p->iface == NULL) {
                continue;
            }
            if (p->failed) {
                retry_read(p, now);
            }
            if (p->len && (p->gen != p->sent_gen || now - p->sent_at >= RESEND_MS)) {
                send_pad(p, MSG_STATE);
            }
            if (now - p->info_at >= INFO_MS) {
                send_info(p, now);
            }
            update_rumble(p, rumble[p->id & 15], now);
        }

        // All pads' buttons together, for the menu and the update prompt.
        uint8_t digital = 0;
        int a = 0, b = 0;
        for (int i = 0; i < MAX_PADS; i++) {
            if (pads[i].iface != NULL && pads[i].len >= 6) {
                digital |= pads[i].report[2];
                a |= pads[i].report[4] > 0x40;
                b |= pads[i].report[5] > 0x40;
            }
        }
        if (menu_update(digital, a, b, now)) {
            continue; // the menu has the controller
        }

        // A downloaded update waits for the user: hold A for 2 s on any pad to install it, B to skip.
        if (update_state == UPDATE_READY) {
            if (b) {
                update_state = UPDATE_NONE;
                DeleteFileA(update_file);
                say("Update skipped; it will be offered again next time.");
            } else if (!a) {
                a_held_since = 0;
            } else if (a_held_since == 0) {
                a_held_since = now ? now : 1;
            } else if (now - a_held_since >= 2000) {
                update_state = UPDATE_INSTALLING;
                say("Installing xidpipe %s...", update_version);
                if (install_xbe(update_file)) {
                    say("Restarting...");
                    relaunch_self();
                }
                say("Install failed; keeping v" XIDPIPE_VERSION ".");
                DeleteFileA(update_file);
                update_state = UPDATE_NONE;
            }
        }
    }
}
