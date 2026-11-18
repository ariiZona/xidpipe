// SPDX-License-Identifier: GPL-3.0-or-later
// xidpipe, Xbox side: forwards raw XID controller reports to the PC app over UDP.
//
// Latency path: USB interrupt -> DPC copies the report and signals `wake` -> main thread
// sends it straight away. Nothing is batched or polled on a timer.
// Reliability: every packet carries the full controller state, and the state is resent
// every RESEND_MS even when nothing changed, so a lost packet is fixed by the next one.
// Rumble comes back from the PC the same way (full state, resent), and stops by itself if
// the PC goes quiet, so a lost packet or a crashed PC can't leave the motors running.

#include <hal/debug.h>
#include <hal/video.h>
#include <windows.h>
#include <xboxkrnl/xboxkrnl.h>
#include <nxdk/net.h>
#include <lwip/netif.h>
#include <lwip/sockets.h>
#include <usbh_lib.h>
#include <usb.h>
#include <hub.h>
#include <string.h>

#define XP_PORT 47100 // keep in sync with pc/src/engine.rs
#define XP_VERSION 1
#define MSG_STATE 1
#define MSG_UNPLUGGED 2
#define MSG_HELLO 0x10
#define MSG_RUMBLE 0x11
#define HEADER 14
#define REPORT_MAX 32
#define MAX_PADS 8
#define RESEND_MS 50
#define PEER_TIMEOUT_MS 3000
#define RETRY_MS 500
#define RUMBLE_TIMEOUT_MS 1000 // the PC resends every 250 ms
#define XID_CLASS 0x58

typedef struct {
    IFACE_T *iface;           // NULL: slot is free
    UTR_T *utr;
    uint8_t id;               // 1-4: front port number, 5+: port unknown
    uint16_t vid, pid;
    volatile uint32_t gen;    // bumped by the DPC for every report
    volatile uint8_t failed;  // the DPC stopped reading after a USB error
    volatile uint8_t len;
    uint8_t report[REPORT_MAX];
    uint32_t sent_gen;
    DWORD sent_at, failed_at;
    UTR_T *out_utr;           // rumble writes; NULL if the pad has no OUT endpoint
    volatile uint8_t out_busy;
    uint16_t rumble[2];       // wanted: left (heavy) motor, right (light) motor
    uint16_t rumble_sent[2];
    DWORD rumble_seen;
} pad_t;

extern struct netif *g_pnetif;

static pad_t pads[MAX_PADS];
static uint32_t seqs[16]; // per pad id; never reset, so the PC can drop stale packets
static KEVENT wake;
static int sock = -1;
static struct sockaddr_in peer;
static int have_peer;
static DWORD peer_seen;

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
    }
    p->sent_at = GetTickCount();
    if (!have_peer) {
        return;
    }

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
    lwip_sendto(sock, b, HEADER + len, 0, (struct sockaddr *)&peer, sizeof(peer));
}

// Runs in the USB DPC. Copy the report, re-arm the read, wake the main thread.
static void on_report(UTR_T *utr)
{
    pad_t *p = (pad_t *)utr->context;

    if (p->iface == NULL) {
        return;
    }
    if (utr->status < 0) {
        p->failed = 1;
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
    }

    utr->xfer_len = 0;
    utr->bIsTransferDone = 0;
    if (usbh_int_xfer(utr) != USBH_OK) {
        p->failed = 1;
    }
    KeSetEvent(&wake, 1, FALSE);
}

// Runs in the USB DPC when a rumble write finished (or failed; the next change retries).
static void on_rumble_done(UTR_T *utr)
{
    ((pad_t *)utr->context)->out_busy = 0;
    KeSetEvent(&wake, 1, FALSE);
}

static void update_rumble(pad_t *p, DWORD now)
{
    if (!have_peer || now - p->rumble_seen > RUMBLE_TIMEOUT_MS) {
        p->rumble[0] = p->rumble[1] = 0;
    }
    if (p->out_utr == NULL || p->out_busy ||
        (p->rumble[0] == p->rumble_sent[0] && p->rumble[1] == p->rumble_sent[1])) {
        return;
    }
    // XID rumble report: id 0, length 6, left u16, right u16
    uint8_t *b = p->out_utr->buff;
    b[0] = 0;
    b[1] = 6;
    b[2] = p->rumble[0];
    b[3] = p->rumble[0] >> 8;
    b[4] = p->rumble[1];
    b[5] = p->rumble[1] >> 8;
    p->out_utr->data_len = 6;
    p->out_utr->xfer_len = 0;
    p->out_utr->bIsTransferDone = 0;
    // Marked sent even if queueing fails, so a pad that rejects rumble isn't retried every loop.
    p->rumble_sent[0] = p->rumble[0];
    p->rumble_sent[1] = p->rumble[1];
    p->out_busy = 1;
    if (usbh_int_xfer(p->out_utr) != USBH_OK) {
        p->out_busy = 0;
    }
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
    int type = 0x01;
    uint8_t *desc = usbh_alloc_mem(16);
    if (desc != NULL) {
        uint32_t got = 0;
        if (usbh_ctrl_xfer(udev, 0xC1, 0x06, 0x4200, iface->if_num, 16, desc, &got, 100) == USBH_OK && got >= 5) {
            type = desc[4];
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
        }
    }
    p->out_utr = out;
    debugPrint("Pad %u connected (%04x:%04x, XID type %02x)\n", p->id, p->vid, p->pid, type);
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
    debugPrint("Pad %u disconnected\n", p->id);
}

// Re-arm a read that stopped after a USB error (3rd-party pads can glitch).
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
    p->failed = 0;
    p->failed_at = 0;
    p->utr->xfer_len = 0;
    p->utr->bIsTransferDone = 0;
    if (usbh_int_xfer(p->utr) != USBH_OK) {
        p->failed = 1;
    }
}

static void poll_net(DWORD now)
{
    uint8_t b[16];
    struct sockaddr_in from;
    socklen_t from_len = sizeof(from);

    int n;
    while ((n = lwip_recvfrom(sock, b, sizeof(b), MSG_DONTWAIT, (struct sockaddr *)&from, &from_len)) >= 4) {
        if (b[0] == 'X' && b[1] == 'P' && b[2] == XP_VERSION && b[3] == MSG_HELLO) {
            if (!have_peer || from.sin_addr.s_addr != peer.sin_addr.s_addr || from.sin_port != peer.sin_port) {
                debugPrint("PC connected: %s\n", ip4addr_ntoa((const ip4_addr_t *)&from.sin_addr));
            }
            peer = from;
            have_peer = 1;
            peer_seen = now;
        } else if (n >= 9 && b[0] == 'X' && b[1] == 'P' && b[2] == XP_VERSION && b[3] == MSG_RUMBLE &&
                   have_peer && from.sin_addr.s_addr == peer.sin_addr.s_addr && from.sin_port == peer.sin_port) {
            // Only the PC we stream to may drive the motors.
            for (int i = 0; i < MAX_PADS; i++) {
                if (pads[i].iface != NULL && pads[i].id == b[4]) {
                    pads[i].rumble[0] = b[5] | b[6] << 8;
                    pads[i].rumble[1] = b[7] | b[8] << 8;
                    pads[i].rumble_seen = now;
                }
            }
        }
        from_len = sizeof(from);
    }

    if (have_peer && now - peer_seen > PEER_TIMEOUT_MS) {
        have_peer = 0;
        debugPrint("PC gone, waiting for it again\n");
    }
}

int main(void)
{
    static UDEV_DRV_T xp_driver = {xp_probe, xp_disconnect, NULL, NULL};

    XVideoSetMode(640, 480, 32, REFRESH_DEFAULT);
    debugPrint("xidpipe\n\nStarting network...\n");

    if (nxNetInit(NULL) != 0) {
        debugPrint("Network failed to start. Check the cable and DHCP, then restart xidpipe.\n");
        for (;;) {
            Sleep(1000);
        }
    }

    sock = lwip_socket(AF_INET, SOCK_DGRAM, 0);
    struct sockaddr_in me;
    memset(&me, 0, sizeof(me));
    me.sin_family = AF_INET;
    me.sin_port = lwip_htons(XP_PORT);
    me.sin_addr.s_addr = INADDR_ANY;
    if (sock < 0 || lwip_bind(sock, (struct sockaddr *)&me, sizeof(me)) != 0) {
        debugPrint("Could not open UDP port %d.\n", XP_PORT);
        for (;;) {
            Sleep(1000);
        }
    }
    debugPrint("Xbox IP: %s  (UDP %d)\n", ip4addr_ntoa(netif_ip4_addr(g_pnetif)), XP_PORT);
    debugPrint("Run xidpipe on the PC. If it can't find the Xbox: xidpipe --xbox <this IP>\n\n");

    KeInitializeEvent(&wake, SynchronizationEvent, FALSE);
    usbh_core_init();
    usbh_register_driver(&xp_driver);

    for (;;) {
        // Woken at once by a new report; the timeout paces hotplug, hellos and resends.
        LARGE_INTEGER timeout;
        timeout.QuadPart = -4 * 10000LL; // 4 ms
        KeWaitForSingleObject(&wake, Executive, KernelMode, FALSE, &timeout);

        usbh_pooling_hubs();
        DWORD now = GetTickCount();
        poll_net(now);

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
            update_rumble(p, now);
        }
    }
}
