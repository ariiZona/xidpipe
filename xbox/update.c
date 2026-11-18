// SPDX-License-Identifier: GPL-3.0-or-later
// Update check: asks GitHub for the latest release over HTTPS (BearSSL, verified against the
// roots in roots.h) and, if it's newer than this build, downloads its default.xbe next to the
// running one. main.c then asks the user before installing it.
// Runs in its own low-priority thread so it never delays controller input.

#include "xidpipe.h"
#include "roots.h"
#include "util.h"
#include <bearssl.h>
#include <lwip/netdb.h>
#include <lwip/netif.h>
#include <lwip/sockets.h>
#include <windows.h>
#include <xboxkrnl/xboxkrnl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define MAX_JSON (512 * 1024)
#define MAX_XBE (16 * 1024 * 1024)
#define IO_TIMEOUT_S 20
#define ROOT_COUNT (sizeof(ROOT_PEMS) / sizeof(ROOT_PEMS[0]))

extern struct netif *g_pnetif;

volatile int update_state = UPDATE_NONE;
char update_version[32];
char update_file[260];

typedef struct {
    uint8_t *data;
    size_t len, cap;
    int failed;
} buf_t;

static void buf_append(void *ctx, const void *src, size_t len)
{
    buf_t *b = ctx;
    if (b->failed) {
        return;
    }
    if (b->len + len > b->cap) {
        size_t cap = (b->len + len) * 2;
        uint8_t *d = realloc(b->data, cap);
        if (d == NULL) {
            b->failed = 1;
            return;
        }
        b->data = d;
        b->cap = cap;
    }
    memcpy(b->data + b->len, src, len);
    b->len += len;
}

static uint8_t *dup_bytes(const void *src, size_t len)
{
    uint8_t *d = malloc(len);
    if (d != NULL) {
        memcpy(d, src, len);
    }
    return d;
}

static br_x509_trust_anchor anchors[ROOT_COUNT];
static size_t anchor_count;

// Turn the PEM roots into BearSSL trust anchors (DN + public key).
static void load_anchors(void)
{
    for (size_t i = 0; i < ROOT_COUNT; i++) {
        buf_t der = {0}, dn = {0};
        br_pem_decoder_context pc;
        const char *p = ROOT_PEMS[i];
        size_t left = strlen(p);
        int inside = 0, done = 0;

        br_pem_decoder_init(&pc);
        while (left > 0 && !done) {
            size_t n = br_pem_decoder_push(&pc, p, left);
            p += n;
            left -= n;
            int ev = br_pem_decoder_event(&pc);
            if (ev == BR_PEM_BEGIN_OBJ) {
                br_pem_decoder_setdest(&pc, buf_append, &der);
                inside = 1;
            } else if (ev == BR_PEM_END_OBJ) {
                done = inside;
            } else if (ev == BR_PEM_ERROR) {
                break;
            }
        }

        br_x509_decoder_context dc;
        br_x509_decoder_init(&dc, buf_append, &dn);
        if (done && !der.failed) {
            br_x509_decoder_push(&dc, der.data, der.len);
        }
        br_x509_pkey *pk = done ? br_x509_decoder_get_pkey(&dc) : NULL;
        if (pk != NULL && br_x509_decoder_last_error(&dc) == 0 && !dn.failed) {
            br_x509_trust_anchor *ta = &anchors[anchor_count];
            ta->dn.data = dn.data;
            ta->dn.len = dn.len;
            ta->flags = BR_X509_TA_CA;
            ta->pkey.key_type = pk->key_type;
            if (pk->key_type == BR_KEYTYPE_RSA) {
                ta->pkey.key.rsa.n = dup_bytes(pk->key.rsa.n, pk->key.rsa.nlen);
                ta->pkey.key.rsa.nlen = pk->key.rsa.nlen;
                ta->pkey.key.rsa.e = dup_bytes(pk->key.rsa.e, pk->key.rsa.elen);
                ta->pkey.key.rsa.elen = pk->key.rsa.elen;
            } else {
                ta->pkey.key.ec.curve = pk->key.ec.curve;
                ta->pkey.key.ec.q = dup_bytes(pk->key.ec.q, pk->key.ec.qlen);
                ta->pkey.key.ec.qlen = pk->key.ec.qlen;
            }
            anchor_count++;
        } else {
            free(dn.data);
        }
        free(der.data);
    }
}

// Certificate dates need the current time. An Xbox whose clock was reset reads ~2001, so never
// use a time earlier than this build.
static void set_time(br_x509_minimal_context *xc)
{
    LARGE_INTEGER now;
    KeQuerySystemTime(&now);
    int64_t t = now.QuadPart / 10000000LL - 11644473600LL;
    int64_t built = date_to_unix(__DATE__);
    if (t < built) {
        t = built;
    }
    br_x509_minimal_set_time(xc, (uint32_t)(t / 86400 + 719528), (uint32_t)(t % 86400));
}

// Gather seed material: there's no hardware RNG on a Pentium III, so hash timing jitter.
static void collect_entropy(uint8_t out[32])
{
    br_sha256_context h;
    br_sha256_init(&h);
    for (int i = 0; i < 512; i++) {
        uint64_t tsc = __builtin_ia32_rdtsc();
        ULONGLONG pc = KeQueryPerformanceCounter();
        br_sha256_update(&h, &tsc, sizeof(tsc));
        br_sha256_update(&h, &pc, sizeof(pc));
        for (volatile int j = 0; j < (int)(tsc & 127); j++) {
        }
    }
    LARGE_INTEGER now;
    KeQuerySystemTime(&now);
    br_sha256_update(&h, &now, sizeof(now));
    br_sha256_update(&h, g_pnetif->hwaddr, g_pnetif->hwaddr_len);
    br_sha256_out(&h, out);
}

static void seed(br_ssl_engine_context *eng)
{
    uint8_t out[32];
    collect_entropy(out);
    br_ssl_engine_inject_entropy(eng, out, sizeof(out));
}

// Replaces BearSSL's sysrng.c (which only knows Unix and Windows RNGs).
static int xbox_seeder(const br_prng_class **ctx)
{
    uint8_t out[32];
    collect_entropy(out);
    (*ctx)->update(ctx, out, sizeof(out));
    return 1;
}

br_prng_seeder br_prng_seeder_system(const char **name)
{
    if (name != NULL) {
        *name = "xbox-jitter";
    }
    return &xbox_seeder;
}

static int wait_fd(int fd, int for_write)
{
    fd_set set;
    FD_ZERO(&set);
    FD_SET(fd, &set);
    struct timeval tv = {IO_TIMEOUT_S, 0};
    return lwip_select(fd + 1, for_write ? NULL : &set, for_write ? &set : NULL, NULL, &tv) > 0;
}

static int sock_read(void *ctx, unsigned char *data, size_t len)
{
    int fd = *(int *)ctx;
    if (!wait_fd(fd, 0)) {
        return -1;
    }
    int n = lwip_recv(fd, data, len, 0);
    return n > 0 ? n : -1;
}

static int sock_write(void *ctx, const unsigned char *data, size_t len)
{
    int fd = *(int *)ctx;
    if (!wait_fd(fd, 1)) {
        return -1;
    }
    int n = lwip_send(fd, data, len, 0);
    return n > 0 ? n : -1;
}

// Connect to host:port. Says why it failed (name lookup or connection) and returns -1 then.
static int tcp_connect(const char *host, int port)
{
    struct hostent *he = lwip_gethostbyname(host);
    if (he == NULL || he->h_addr_list[0] == NULL) {
        say("Update check: can't look up %s. Is a DNS server set on the Xbox?", host);
        return -1;
    }
    int fd = lwip_socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        say("Update check: no free network socket");
        return -1;
    }
    struct sockaddr_in a;
    memset(&a, 0, sizeof(a));
    a.sin_family = AF_INET;
    a.sin_port = lwip_htons(port);
    memcpy(&a.sin_addr, he->h_addr_list[0], 4);
    if (lwip_connect(fd, (struct sockaddr *)&a, sizeof(a)) != 0) {
        say("Update check: can't connect to %s (%s)", host, ip4addr_ntoa((const ip4_addr_t *)&a.sin_addr));
        lwip_close(fd);
        return -1;
    }
    return fd;
}

static br_ssl_client_context sc;
static br_x509_minimal_context xc;
static unsigned char iobuf[BR_SSL_BUFSIZE_BIDI];
static char url_buf[4096], path_buf[4096], req_buf[4608];

// GET an https URL, following redirects. On HTTP 200 with a complete body, returns 1 and the
// body in `out` (malloc'd, with a NUL after it). Every failure says why on screen.
static int https_get(const char *url, buf_t *out, size_t max)
{
    snprintf(url_buf, sizeof(url_buf), "%s", url);
    for (int redirects = 0; redirects < 6; redirects++) {
        char host[256];
        const char *h = url_buf + 8, *slash = strchr(h, '/');
        size_t hl = slash ? (size_t)(slash - h) : strlen(h);
        if (strncmp(url_buf, "https://", 8) != 0 || hl == 0 || hl >= sizeof(host)) {
            say("Update check: unusable address %.60s", url_buf);
            return 0;
        }
        memcpy(host, h, hl);
        host[hl] = 0;
        snprintf(path_buf, sizeof(path_buf), "%s", slash ? slash : "/");

        int fd = tcp_connect(host, 443);
        if (fd < 0) {
            return 0;
        }
        br_ssl_client_init_full(&sc, &xc, anchors, anchor_count);
        set_time(&xc);
        br_ssl_engine_set_buffer(&sc.eng, iobuf, sizeof(iobuf), 1);
        seed(&sc.eng);
        br_ssl_client_reset(&sc, host, 0);
        br_sslio_context io;
        br_sslio_init(&io, &sc.eng, sock_read, &fd, sock_write, &fd);

        int rl = snprintf(req_buf, sizeof(req_buf),
                          "GET %s HTTP/1.1\r\nHost: %s\r\nUser-Agent: xidpipe-xbox/" XIDPIPE_VERSION
                          "\r\nAccept: */*\r\nConnection: close\r\n\r\n",
                          path_buf, host);
        buf_t resp = {0};
        int sent = rl > 0 && rl < (int)sizeof(req_buf) && br_sslio_write_all(&io, req_buf, rl) == 0 &&
                   br_sslio_flush(&io) == 0;
        int too_big = 0;
        while (sent) {
            static unsigned char chunk[4096];
            int n = br_sslio_read(&io, chunk, sizeof(chunk));
            if (n < 0) {
                break;
            }
            if (resp.len + (size_t)n > max + 65536) {
                too_big = 1;
                break;
            }
            buf_append(&resp, chunk, n);
        }
        // TLS error codes are listed in bearssl/inc/bearssl_ssl.h (BR_ERR_*) and bearssl_x509.h.
        int tls_err = br_ssl_engine_last_error(&sc.eng);
        lwip_close(fd);
        unsigned long got = (unsigned long)resp.len;
        buf_append(&resp, "", 1); // NUL-terminate for string parsing
        if (!sent || resp.failed || too_big || got == 0) {
            say("Update check: %s: %s (TLS error %d, %lu bytes)", host,
                !sent ? "secure connection failed" : too_big ? "response too big" : resp.failed ? "out of memory" : "no answer",
                tls_err, got);
            free(resp.data);
            return 0;
        }
        resp.len--;

        char *r = (char *)resp.data;
        char *body = strstr(r, "\r\n\r\n");
        int status = 0;
        if (body == NULL || sscanf(r, "HTTP/%*s %d", &status) != 1) {
            say("Update check: %s sent an unreadable answer (%lu bytes, TLS error %d)", host, got, tls_err);
            free(resp.data);
            return 0;
        }
        body[2] = 0; // end the header block after the last header's CRLF
        body += 4;
        size_t blen = resp.len - (size_t)(body - r);

        if (status >= 300 && status < 400) {
            int found = http_header(r, "location", url_buf, sizeof(url_buf));
            free(resp.data);
            if (!found) {
                say("Update check: %s redirected (HTTP %d) without saying where", host, status);
                return 0;
            }
            continue;
        }
        if (status != 200) {
            say("Update check: %s answered HTTP %d", host, status);
            free(resp.data);
            return 0;
        }
        char val[64];
        unsigned long expected = 0;
        int complete;
        if (http_header(r, "transfer-encoding", val, sizeof(val)) && strstr(val, "chunked") != NULL) {
            complete = http_dechunk(body, blen, &blen);
        } else if (http_header(r, "content-length", val, sizeof(val))) {
            expected = strtoul(val, NULL, 10);
            complete = expected == blen;
        } else {
            complete = tls_err == 0; // no length given: only a clean close proves it's all there
        }
        if (!complete) {
            // never use a partial file
            say("Update check: answer from %s was cut off (%lu of %lu bytes, TLS error %d)", host,
                (unsigned long)blen, expected, tls_err);
            free(resp.data);
            return 0;
        }
        if (blen > max) {
            say("Update check: answer from %s is too big (%lu bytes)", host, (unsigned long)blen);
            free(resp.data);
            return 0;
        }
        memmove(resp.data, body, blen);
        resp.data[blen] = 0;
        out->data = resp.data;
        out->len = blen;
        return 1;
    }
    say("Update check: too many redirects");
    return 0;
}

static int write_file(const char *path, const uint8_t *data, uint32_t len)
{
    HANDLE f = CreateFileA(path, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (f == INVALID_HANDLE_VALUE) {
        return 0;
    }
    DWORD written = 0;
    BOOL ok = WriteFile(f, data, len, &written, NULL) && written == len;
    CloseHandle(f);
    if (!ok) {
        DeleteFileA(path);
    }
    return ok;
}

static DWORD WINAPI update_thread(LPVOID arg)
{
    (void)arg;
    load_anchors();
    if (anchor_count == 0) {
        say("Update check: no usable root certificates.");
        return 0;
    }

    buf_t json = {0};
    char tag[32];
    if (!https_get("https://api.github.com/repos/" XIDPIPE_REPO "/releases/latest", &json, MAX_JSON)) {
        say("Update check failed, see the line above.");
        return 0;
    }
    int found = json_string((char *)json.data, "tag_name", tag, sizeof(tag));
    free(json.data);
    if (!found) {
        say("Update check: no release found.");
        return 0;
    }
    if (!version_newer(tag, XIDPIPE_VERSION)) {
        say("xidpipe is up to date (v%s).", XIDPIPE_VERSION);
        return 0;
    }

    say("Downloading xidpipe %s...", tag);
    char url[512];
    snprintf(url, sizeof(url), "https://github.com/%s/releases/download/%s/xidpipe-%s-default.xbe", XIDPIPE_REPO,
             tag, tag);
    buf_t xbe = {0};
    if (!https_get(url, &xbe, MAX_XBE) || !looks_like_xbe(xbe.data, xbe.len)) {
        say("Downloading xidpipe %s failed; keeping v%s.", tag, XIDPIPE_VERSION);
        free(xbe.data);
        return 0;
    }
    snprintf(update_file, sizeof(update_file), "%s.new", self_xbe_path());
    int saved = write_file(update_file, xbe.data, xbe.len);
    free(xbe.data);
    if (!saved) {
        say("Couldn't save the update next to xidpipe.");
        return 0;
    }
    snprintf(update_version, sizeof(update_version), "%s", tag);
    update_state = UPDATE_READY;
    say("xidpipe %s is ready. Hold A for 2 s to install and restart, or press B to skip.", tag);
    return 0;
}

void update_start(void)
{
    if (marker_exists("no-update-check")) {
        say("Update check is off (no-update-check file).");
        return;
    }
    HANDLE t = CreateThread(NULL, 128 * 1024, update_thread, NULL, 0, NULL);
    if (t != NULL) {
        SetThreadPriority(t, THREAD_PRIORITY_BELOW_NORMAL);
        CloseHandle(t);
    }
}
