// SPDX-License-Identifier: GPL-3.0-or-later
// FTP server for the whole Xbox, the way Xbox dashboards do it. Modelled on xemu-dashboard's FTP
// server (https://github.com/xemu-project/xemu-dashboard, lib/ftpd; MIT, Ryzee119, based on Jean-Michel
// Gallego's Apache-2.0 FTP server): user "xbox", password "xbox", "/" lists the drives and
// "/E/Apps" is E:\Apps. Several clients at once (FileZilla opens extra connections for transfers),
// passive mode only. Uploads land in two page-aligned 128 KB buffers that a writer thread writes
// out unbuffered, so the network never waits for the disk (the same trick xemu-dashboard uses).
//
// Safety: an upload goes to "<file>.part" and only replaces the file once it's complete. The
// running XBE can only be replaced (the new one is checked, installed and started), never deleted
// or renamed. Like any Xbox FTP server, anyone on the network who knows xbox/xbox gets full access
// to the drives; an empty "no-ftp" file in xidpipe's folder turns it off.
// Each client runs in its own low-priority thread so it never delays controller input.

#include "xidpipe.h"
#include "util.h"
#include <lwip/sockets.h>
#include <windows.h>
#include <xboxkrnl/xboxkrnl.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define FTP_PORT 21
#define FTP_USER "xbox"
#define FTP_PASS "xbox"
#define MAX_CLIENTS 4
#define IDLE_TIMEOUT_S 300
#define DATA_TIMEOUT_S 30
#define WRITE_CHUNK (128 * 1024) // multiple of the sector size, for unbuffered writes
#define READ_CHUNK (64 * 1024)
#define PAGE 4096

// Drives listed at "/", in this order (only the ones that can be read right now are shown).
static const char DRIVES[] = "CDEFGXYZ";

static volatile long clients;
static volatile int enabled;

int ftp_is_enabled(void)
{
    return enabled;
}

void ftp_set_enabled(int on)
{
    set_marker("no-ftp", !on);
    enabled = on;
}

typedef struct {
    int ctrl;
    int pasv; // listening data socket, -1 if none
    int user_ok, logged_in;
    uint32_t rest; // REST offset for the next RETR
    char cwd[XP_PATH_MAX];
    char rnfr[XP_PATH_MAX]; // DOS path waiting for RNTO
    char line[600];
    size_t line_len;
} session_t;

static int send_all(int fd, const void *data, size_t len)
{
    const char *p = data;
    while (len > 0) {
        int n = lwip_send(fd, p, len, 0);
        if (n <= 0) {
            return 0;
        }
        p += n;
        len -= n;
    }
    return 1;
}

static void reply(session_t *s, const char *fmt, ...)
{
    char b[700];
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(b, sizeof(b) - 2, fmt, ap);
    va_end(ap);
    if (n < 0 || n > (int)sizeof(b) - 3) {
        n = (int)sizeof(b) - 3;
    }
    b[n] = '\r';
    b[n + 1] = '\n';
    send_all(s->ctrl, b, n + 2);
}

static int readable(int fd, int seconds)
{
    fd_set set;
    FD_ZERO(&set);
    FD_SET(fd, &set);
    struct timeval tv = {seconds, 0};
    return lwip_select(fd + 1, &set, NULL, NULL, &tv) > 0;
}

static int resolve(const session_t *s, const char *arg, char *ftp_out, char *dos_out)
{
    return ftp_resolve(s->cwd, arg, ftp_out, dos_out);
}

static int is_self(const char *dos)
{
    return self_xbe_real_path()[0] != 0 && _stricmp(dos, self_xbe_real_path()) == 0;
}

static int is_dir(const char *dos)
{
    DWORD a = GetFileAttributesA(dos);
    return a != INVALID_FILE_ATTRIBUTES && (a & FILE_ATTRIBUTE_DIRECTORY);
}

static int drive_ready(char letter)
{
    char root[4] = {letter, ':', '\\', 0};
    return GetFileAttributesA(root) != INVALID_FILE_ATTRIBUTES;
}

static TIME_FIELDS time_fields(FILETIME ft)
{
    TIME_FIELDS tf;
    LARGE_INTEGER t;
    t.LowPart = ft.dwLowDateTime;
    t.HighPart = (LONG)ft.dwHighDateTime;
    RtlTimeToTimeFields(&t, &tf);
    return tf;
}

static int find_one(const char *dos, WIN32_FIND_DATAA *fd)
{
    HANDLE h = FindFirstFileA(dos, fd);
    if (h == INVALID_HANDLE_VALUE) {
        return 0;
    }
    FindClose(h);
    return 1;
}

static void close_pasv(session_t *s)
{
    if (s->pasv >= 0) {
        lwip_close(s->pasv);
        s->pasv = -1;
    }
}

static int open_pasv(session_t *s, int *port)
{
    close_pasv(s);
    int fd = lwip_socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in a;
    memset(&a, 0, sizeof(a));
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = INADDR_ANY;
    socklen_t al = sizeof(a);
    if (fd < 0 || lwip_bind(fd, (struct sockaddr *)&a, sizeof(a)) != 0 || lwip_listen(fd, 1) != 0 ||
        lwip_getsockname(fd, (struct sockaddr *)&a, &al) != 0) {
        if (fd >= 0) {
            lwip_close(fd);
        }
        return 0;
    }
    s->pasv = fd;
    *port = lwip_ntohs(a.sin_port);
    return 1;
}

static int accept_data(session_t *s)
{
    if (s->pasv < 0) {
        reply(s, "425 Use PASV or EPSV first");
        return -1;
    }
    int fd = readable(s->pasv, DATA_TIMEOUT_S) ? lwip_accept(s->pasv, NULL, NULL) : -1;
    close_pasv(s);
    if (fd < 0) {
        reply(s, "425 No data connection");
    }
    return fd;
}

static void list_line(int fd, int names_only, int dir, uint32_t size, FILETIME ft, const char *name)
{
    static const char *const months[] = {"Jan", "Feb", "Mar", "Apr", "May", "Jun",
                                         "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"};
    char line[400];
    int n;
    if (names_only) {
        n = snprintf(line, sizeof(line), "%s\r\n", name);
    } else {
        TIME_FIELDS tf = time_fields(ft);
        int m = tf.Month >= 1 && tf.Month <= 12 ? tf.Month - 1 : 0;
        n = snprintf(line, sizeof(line), "%crw-r--r-- 1 xbox xbox %10lu %s %2d %5d %s\r\n", dir ? 'd' : '-',
                     (unsigned long)size, months[m], tf.Day, tf.Year, name);
    }
    if (n > 0) {
        send_all(fd, line, n < (int)sizeof(line) ? (size_t)n : sizeof(line) - 1);
    }
}

static void cmd_list(session_t *s, const char *arg, int names_only)
{
    // Skip options some clients send, like "-la".
    while (arg[0] == '-') {
        const char *sp = strchr(arg, ' ');
        arg = sp ? sp + 1 : "";
    }
    char ftp[XP_PATH_MAX], dos[XP_PATH_MAX], pattern[XP_PATH_MAX + 4];
    int kind = resolve(s, arg[0] ? arg : ".", ftp, dos);
    if (kind == FTP_BAD || (kind == FTP_PATH && !is_dir(dos))) {
        reply(s, "550 No such folder");
        return;
    }
    int fd = accept_data(s);
    if (fd < 0) {
        return;
    }
    reply(s, "150 Listing %s", ftp);
    if (kind == FTP_ROOT) {
        FILETIME y2k = {0x256d4000, 0x01bf53eb}; // 2000-01-01, like other Xbox FTP servers
        for (const char *d = DRIVES; *d; d++) {
            if (drive_ready(*d)) {
                char name[2] = {*d, 0};
                list_line(fd, names_only, 1, 0, y2k, name);
            }
        }
    } else {
        snprintf(pattern, sizeof(pattern), "%s%s*", dos, dos[strlen(dos) - 1] == '\\' ? "" : "\\");
        WIN32_FIND_DATAA fd_data;
        HANDLE h = FindFirstFileA(pattern, &fd_data);
        if (h != INVALID_HANDLE_VALUE) {
            do {
                if (strcmp(fd_data.cFileName, ".") && strcmp(fd_data.cFileName, "..")) {
                    list_line(fd, names_only, (fd_data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0,
                              fd_data.nFileSizeLow, fd_data.ftLastWriteTime, fd_data.cFileName);
                }
            } while (FindNextFileA(h, &fd_data));
            FindClose(h);
        }
    }
    lwip_close(fd);
    reply(s, "226 Done");
}

static void cmd_retr(session_t *s, const char *arg)
{
    char ftp[XP_PATH_MAX], dos[XP_PATH_MAX];
    HANDLE f = resolve(s, arg, ftp, dos) == FTP_PATH
                   ? CreateFileA(dos, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, NULL)
                   : INVALID_HANDLE_VALUE;
    uint32_t rest = s->rest;
    s->rest = 0;
    if (f == INVALID_HANDLE_VALUE) {
        reply(s, "550 Can't open %s", arg);
        return;
    }
    char *buf = malloc(READ_CHUNK);
    if (buf == NULL || (rest && SetFilePointer(f, (LONG)rest, NULL, FILE_BEGIN) == INVALID_SET_FILE_POINTER)) {
        free(buf);
        CloseHandle(f);
        reply(s, "451 Can't read %s", ftp);
        return;
    }
    int fd = accept_data(s);
    if (fd < 0) {
        free(buf);
        CloseHandle(f);
        return;
    }
    reply(s, "150 Sending %s", ftp);
    DWORD n;
    int ok = 1;
    while (ok && ReadFile(f, buf, READ_CHUNK, &n, NULL) && n > 0) {
        ok = send_all(fd, buf, n);
    }
    free(buf);
    CloseHandle(f);
    lwip_close(fd);
    reply(s, ok ? "226 Done" : "426 Transfer aborted");
}

// Writes full chunks in the background while the next one is being received.
typedef struct {
    HANDLE file, go, done;
    const uint8_t *data;
    DWORD len;
    volatile int ok, quit;
} writer_t;

static DWORD WINAPI writer_thread(LPVOID arg)
{
    writer_t *w = arg;
    for (;;) {
        WaitForSingleObject(w->go, INFINITE);
        if (w->quit) {
            break;
        }
        DWORD n = 0;
        if (!WriteFile(w->file, w->data, w->len, &n, NULL) || n != w->len) {
            w->ok = 0;
        }
        SetEvent(w->done);
    }
    SetEvent(w->done);
    return 0;
}

// Receive an upload into `part`. Returns the size, or -1 if it was cut off or couldn't be written.
static long long receive_file(int fd, const char *part, uint8_t head[16])
{
    uint8_t *raw = malloc(2 * WRITE_CHUNK + PAGE);
    HANDLE f = CreateFileA(part, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS,
                           FILE_ATTRIBUTE_NORMAL | FILE_FLAG_NO_BUFFERING, NULL);
    writer_t w = {f, CreateEventA(NULL, FALSE, FALSE, NULL), CreateEventA(NULL, FALSE, TRUE, NULL), NULL, 0, 1, 0};
    HANDLE thread = (raw && f != INVALID_HANDLE_VALUE && w.go && w.done)
                        ? CreateThread(NULL, 16 * 1024, writer_thread, &w, 0, NULL)
                        : NULL;
    long long total = -1;
    if (thread != NULL) {
        SetThreadPriority(thread, THREAD_PRIORITY_BELOW_NORMAL);
        uint8_t *bufs[2];
        bufs[0] = (uint8_t *)(((uintptr_t)raw + PAGE - 1) & ~(uintptr_t)(PAGE - 1));
        bufs[1] = bufs[0] + WRITE_CHUNK;
        int cur = 0, n = -1;
        DWORD fill = 0;
        total = 0;
        while (w.ok && readable(fd, DATA_TIMEOUT_S) &&
               (n = lwip_recv(fd, bufs[cur] + fill, WRITE_CHUNK - fill, 0)) > 0) {
            if (total < 16) {
                memcpy(head + total, bufs[cur] + fill, (size_t)(n < 16 - total ? n : 16 - total));
            }
            fill += n;
            total += n;
            if (fill == WRITE_CHUNK) {
                WaitForSingleObject(w.done, INFINITE); // the other buffer is free again
                w.data = bufs[cur];
                w.len = WRITE_CHUNK;
                SetEvent(w.go);
                cur ^= 1;
                fill = 0;
            }
        }
        int complete = w.ok && n == 0; // the client closed the data connection: all received
        WaitForSingleObject(w.done, INFINITE);
        if (complete && fill > 0) {
            // Unbuffered writes must be whole sectors: pad, then cut the file to its real size.
            DWORD padded = (fill + PAGE - 1) & ~(DWORD)(PAGE - 1), written = 0;
            memset(bufs[cur] + fill, 0, padded - fill);
            complete = WriteFile(f, bufs[cur], padded, &written, NULL) && written == padded;
        }
        w.quit = 1;
        SetEvent(w.go);
        WaitForSingleObject(thread, INFINITE);
        CloseHandle(thread);
        if (complete && w.ok) {
            IO_STATUS_BLOCK io;
            FILE_END_OF_FILE_INFORMATION eof;
            FILE_ALLOCATION_INFORMATION alloc;
            eof.EndOfFile.QuadPart = total;
            alloc.AllocationSize.QuadPart = total;
            if (!NT_SUCCESS(NtSetInformationFile(f, &io, &eof, sizeof(eof), FileEndOfFileInformation))) {
                complete = 0;
            }
            NtSetInformationFile(f, &io, &alloc, sizeof(alloc), FileAllocationInformation);
        }
        if (!complete || !w.ok) {
            total = -1;
        }
    }
    if (w.go) {
        CloseHandle(w.go);
    }
    if (w.done) {
        CloseHandle(w.done);
    }
    if (f != INVALID_HANDLE_VALUE) {
        CloseHandle(f);
        if (total < 0) {
            DeleteFileA(part);
        }
    }
    free(raw);
    return total;
}

// A cut-off upload never replaces a working file. A new copy of the running XBE is checked,
// installed and started.
static void cmd_stor(session_t *s, const char *arg)
{
    char ftp[XP_PATH_MAX], dos[XP_PATH_MAX], part[XP_PATH_MAX + 8];
    if (s->rest) {
        s->rest = 0;
        reply(s, "550 Resuming uploads isn't supported");
        return;
    }
    if (resolve(s, arg, ftp, dos) != FTP_PATH || is_dir(dos)) {
        reply(s, "553 Bad file name");
        return;
    }
    int self = is_self(dos);
    snprintf(part, sizeof(part), "%s.part", self ? self_xbe_path() : dos);
    int fd = accept_data(s);
    if (fd < 0) {
        return;
    }
    reply(s, "150 Receiving %s", ftp);
    uint8_t head[16] = {0};
    long long total = receive_file(fd, part, head);
    lwip_close(fd);
    if (total < 0) {
        reply(s, "426 Upload incomplete or couldn't be written; nothing was changed");
        return;
    }
    if (self) {
        if (!looks_like_xbe(head, (uint32_t)total) || !install_xbe(part)) {
            DeleteFileA(part);
            reply(s, "550 Not a valid XBE, or it couldn't be installed; the old one is still in place");
            return;
        }
        reply(s, "226 Installed; xidpipe is restarting");
        say("New xidpipe uploaded over FTP. Restarting...");
        relaunch_self();
    }
    DeleteFileA(dos);
    if (!MoveFileA(part, dos)) {
        DeleteFileA(part);
        reply(s, "550 Couldn't save %s", ftp);
        return;
    }
    reply(s, "226 Saved %s", ftp);
}

static void handle(session_t *s, char *cmd, char *arg)
{
    char ftp[XP_PATH_MAX], dos[XP_PATH_MAX];
    WIN32_FIND_DATAA fd;

    if (!_stricmp(cmd, "USER")) {
        s->user_ok = !_stricmp(arg, FTP_USER);
        s->logged_in = 0;
        reply(s, "331 Password required");
        return;
    }
    if (!_stricmp(cmd, "PASS")) {
        s->logged_in = s->user_ok && !strcmp(arg, FTP_PASS);
        reply(s, s->logged_in ? "230 Logged in" : "530 Login incorrect");
        return;
    }
    if (!_stricmp(cmd, "SYST")) {
        reply(s, "215 UNIX Type: L8");
        return;
    }
    if (!_stricmp(cmd, "FEAT")) {
        reply(s, "211-Features:\r\n SIZE\r\n MDTM\r\n REST STREAM\r\n EPSV\r\n PASV\r\n UTF8\r\n211 End");
        return;
    }
    if (!_stricmp(cmd, "OPTS") || !_stricmp(cmd, "NOOP")) {
        reply(s, "200 OK");
        return;
    }
    if (!s->logged_in) {
        reply(s, "530 Log in with USER xbox, PASS xbox");
        return;
    }

    if (!_stricmp(cmd, "TYPE") || !_stricmp(cmd, "MODE") || !_stricmp(cmd, "STRU")) {
        reply(s, "200 OK"); // transfers are always binary
    } else if (!_stricmp(cmd, "PWD") || !_stricmp(cmd, "XPWD")) {
        reply(s, "257 \"%s\" is the current folder", s->cwd);
    } else if (!_stricmp(cmd, "CWD") || !_stricmp(cmd, "XCWD") || !_stricmp(cmd, "CDUP")) {
        int kind = resolve(s, !_stricmp(cmd, "CDUP") ? ".." : arg, ftp, dos);
        if (kind == FTP_ROOT || (kind == FTP_PATH && is_dir(dos))) {
            snprintf(s->cwd, sizeof(s->cwd), "%s", ftp);
            reply(s, "250 Now in %s", s->cwd);
        } else {
            reply(s, "550 No such folder");
        }
    } else if (!_stricmp(cmd, "PASV") || !_stricmp(cmd, "EPSV")) {
        int port;
        struct sockaddr_in me;
        socklen_t ml = sizeof(me);
        if (!open_pasv(s, &port) || lwip_getsockname(s->ctrl, (struct sockaddr *)&me, &ml) != 0) {
            reply(s, "425 Can't open a data port");
        } else if (!_stricmp(cmd, "EPSV")) {
            reply(s, "229 Entering Extended Passive Mode (|||%d|)", port);
        } else {
            uint32_t ip = me.sin_addr.s_addr; // the address the client reached us on
            reply(s, "227 Entering Passive Mode (%u,%u,%u,%u,%d,%d)", ip & 0xff, (ip >> 8) & 0xff, (ip >> 16) & 0xff,
                  ip >> 24, port >> 8, port & 0xff);
        }
    } else if (!_stricmp(cmd, "LIST") || !_stricmp(cmd, "NLST")) {
        cmd_list(s, arg, !_stricmp(cmd, "NLST"));
    } else if (!_stricmp(cmd, "REST")) {
        s->rest = (uint32_t)strtoul(arg, NULL, 10);
        reply(s, "350 Restarting at %lu", (unsigned long)s->rest);
    } else if (!_stricmp(cmd, "RETR")) {
        cmd_retr(s, arg);
    } else if (!_stricmp(cmd, "STOR")) {
        cmd_stor(s, arg);
    } else if (!_stricmp(cmd, "SIZE")) {
        if (resolve(s, arg, ftp, dos) == FTP_PATH && find_one(dos, &fd) &&
            !(fd.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY)) {
            reply(s, "213 %lu", (unsigned long)fd.nFileSizeLow);
        } else {
            reply(s, "550 No such file");
        }
    } else if (!_stricmp(cmd, "MDTM")) {
        if (resolve(s, arg, ftp, dos) == FTP_PATH && find_one(dos, &fd)) {
            TIME_FIELDS t = time_fields(fd.ftLastWriteTime);
            reply(s, "213 %04d%02d%02d%02d%02d%02d", t.Year, t.Month, t.Day, t.Hour, t.Minute, t.Second);
        } else {
            reply(s, "550 No such file");
        }
    } else if (!_stricmp(cmd, "DELE")) {
        int ok = resolve(s, arg, ftp, dos) == FTP_PATH && !is_self(dos) && DeleteFileA(dos);
        reply(s, ok ? "250 Deleted" : "550 Can't delete that (the running xidpipe can only be replaced)");
    } else if (!_stricmp(cmd, "MKD") || !_stricmp(cmd, "XMKD")) {
        if (resolve(s, arg, ftp, dos) == FTP_PATH && CreateDirectoryA(dos, NULL)) {
            reply(s, "257 \"%s\" created", ftp);
        } else {
            reply(s, "550 Can't create that folder");
        }
    } else if (!_stricmp(cmd, "RMD") || !_stricmp(cmd, "XRMD")) {
        // A drive itself ("/E") resolves to "E:\" and can't be removed.
        int ok = resolve(s, arg, ftp, dos) == FTP_PATH && strlen(dos) > 3 && RemoveDirectoryA(dos);
        reply(s, ok ? "250 Removed" : "550 Can't remove that folder");
    } else if (!_stricmp(cmd, "RNFR")) {
        if (resolve(s, arg, ftp, dos) == FTP_PATH && strlen(dos) > 3 && !is_self(dos) &&
            GetFileAttributesA(dos) != INVALID_FILE_ATTRIBUTES) {
            snprintf(s->rnfr, sizeof(s->rnfr), "%s", dos);
            reply(s, "350 Ready for RNTO");
        } else {
            reply(s, "550 Can't rename that");
        }
    } else if (!_stricmp(cmd, "RNTO")) {
        int ok = s->rnfr[0] && resolve(s, arg, ftp, dos) == FTP_PATH && !is_self(dos) && MoveFileA(s->rnfr, dos);
        s->rnfr[0] = 0;
        reply(s, ok ? "250 Renamed" : "550 Can't rename that");
    } else if (!_stricmp(cmd, "PORT") || !_stricmp(cmd, "EPRT")) {
        reply(s, "502 Use passive mode");
    } else {
        reply(s, "502 Not supported");
    }
}

static void serve(session_t *s)
{
    reply(s, "220 xidpipe " XIDPIPE_VERSION " FTP ready (user xbox, password xbox)");
    for (;;) {
        // Wake every second, so turning the server off in the menu disconnects clients.
        int idle = 0;
        while (!readable(s->ctrl, 1)) {
            if (!enabled) {
                reply(s, "421 The FTP server was turned off");
                return;
            }
            if (++idle >= IDLE_TIMEOUT_S) {
                reply(s, "421 Idle too long");
                return;
            }
        }
        int n = lwip_recv(s->ctrl, s->line + s->line_len, sizeof(s->line) - 1 - s->line_len, 0);
        if (n <= 0) {
            return;
        }
        s->line_len += n;
        s->line[s->line_len] = 0;
        char *eol;
        while ((eol = strchr(s->line, '\n')) != NULL) {
            *eol = 0;
            if (eol > s->line && eol[-1] == '\r') {
                eol[-1] = 0;
            }
            char *cmd = s->line, *arg = strchr(s->line, ' ');
            if (arg != NULL) {
                *arg++ = 0;
            } else {
                arg = "";
            }
            if (!_stricmp(cmd, "QUIT")) {
                reply(s, "221 Bye");
                return;
            }
            handle(s, cmd, arg);
            size_t used = (size_t)(eol + 1 - s->line);
            memmove(s->line, eol + 1, s->line_len - used + 1);
            s->line_len -= used;
        }
        if (s->line_len >= sizeof(s->line) - 1) {
            s->line_len = 0; // overlong line: drop it
        }
    }
}

static DWORD WINAPI session_thread(LPVOID arg)
{
    session_t *s = arg;
    serve(s);
    close_pasv(s);
    lwip_close(s->ctrl);
    free(s);
    __atomic_sub_fetch(&clients, 1, __ATOMIC_SEQ_CST);
    return 0;
}

static int open_listener(void)
{
    int lfd = lwip_socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in a;
    memset(&a, 0, sizeof(a));
    a.sin_family = AF_INET;
    a.sin_port = lwip_htons(FTP_PORT);
    a.sin_addr.s_addr = INADDR_ANY;
    if (lfd < 0 || lwip_bind(lfd, (struct sockaddr *)&a, sizeof(a)) != 0 || lwip_listen(lfd, MAX_CLIENTS) != 0) {
        if (lfd >= 0) {
            lwip_close(lfd);
        }
        return -1;
    }
    return lfd;
}

// Listens while the server is on; the menu can switch it at any time.
static DWORD WINAPI ftp_thread(LPVOID arg)
{
    (void)arg;
    int lfd = -1, failed = 0;
    for (;;) {
        if (enabled && lfd < 0 && !failed) {
            lfd = open_listener();
            if (lfd < 0) {
                failed = 1; // tell once; retried after the next off/on
                say("FTP server couldn't start on port %d.", FTP_PORT);
            } else {
                say("FTP server on port %d: user xbox, password xbox.", FTP_PORT);
            }
        } else if (!enabled && (lfd >= 0 || failed)) {
            if (lfd >= 0) {
                lwip_close(lfd);
                lfd = -1;
                say("FTP server is off.");
            }
            failed = 0;
        }
        if (lfd < 0 || !readable(lfd, 1)) {
            if (lfd < 0) {
                Sleep(500);
            }
            continue;
        }
        int c = lwip_accept(lfd, NULL, NULL);
        if (c < 0) {
            Sleep(100);
            continue;
        }
        session_t *s = calloc(1, sizeof(*s));
        HANDLE t = NULL;
        if (s != NULL && __atomic_add_fetch(&clients, 1, __ATOMIC_SEQ_CST) <= MAX_CLIENTS) {
            s->ctrl = c;
            s->pasv = -1;
            strcpy(s->cwd, "/");
            t = CreateThread(NULL, 64 * 1024, session_thread, s, 0, NULL);
        }
        if (t != NULL) {
            SetThreadPriority(t, THREAD_PRIORITY_BELOW_NORMAL);
            CloseHandle(t);
            continue;
        }
        if (s != NULL) {
            __atomic_sub_fetch(&clients, 1, __ATOMIC_SEQ_CST);
            free(s);
        }
        send_all(c, "421 Too many connections\r\n", 26);
        lwip_close(c);
    }
}

void ftp_start(void)
{
    enabled = !marker_exists("no-ftp");
    if (!enabled) {
        say("FTP server is off (turn it on in the menu: hold Back for 10 s).");
    }
    HANDLE t = CreateThread(NULL, 32 * 1024, ftp_thread, NULL, 0, NULL);
    if (t != NULL) {
        SetThreadPriority(t, THREAD_PRIORITY_BELOW_NORMAL);
        CloseHandle(t);
    }
}
