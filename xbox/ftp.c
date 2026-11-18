// SPDX-License-Identifier: GPL-3.0-or-later
// Tiny FTP server for the folder the XBE runs from (the D: drive), so xidpipe can be updated
// while it runs. One client at a time, passive mode only, any login accepted.
// Uploading a new copy of the running XBE checks it, installs it and restarts xidpipe.
// It can't reach anything outside that folder, but anyone on the LAN can use it: create an
// empty file named "no-ftp" in the folder to turn it off.
// Runs in its own low-priority thread so it never delays controller input.

#include "xidpipe.h"
#include "util.h"
#include <lwip/netif.h>
#include <lwip/sockets.h>
#include <windows.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define FTP_PORT 21
#define IDLE_TIMEOUT_S 300
#define DATA_TIMEOUT_S 30
#define PATH_MAX_LEN XP_PATH_MAX

extern struct netif *g_pnetif;

typedef struct {
    int ctrl;
    int pasv; // listening data socket, -1 if none
    char cwd[PATH_MAX_LEN]; // FTP path: "/" or "/a/b"
    char rnfr[PATH_MAX_LEN]; // DOS path waiting for RNTO
    char line[600];
    size_t line_len;
} session_t;

static char io[16384];

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

// Resolve an FTP path (absolute, or relative to the current folder) to an FTP path and a
// DOS path on D:. Nothing can climb above the root (see util.c).
static int resolve(const session_t *s, const char *arg, char *ftp_out, char *dos_out)
{
    return ftp_resolve(s->cwd, arg, ftp_out, dos_out);
}

static int is_self(const char *dos)
{
    return _stricmp(dos, self_xbe_path()) == 0;
}

static int is_dir(const char *dos)
{
    DWORD a = GetFileAttributesA(dos);
    return a != INVALID_FILE_ATTRIBUTES && (a & FILE_ATTRIBUTE_DIRECTORY);
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
    a.sin_port = 0;
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

static void cmd_list(session_t *s, const char *arg, int names_only)
{
    // Clients often pass options like "-la"; list the current folder then.
    char ftp[PATH_MAX_LEN], dos[PATH_MAX_LEN], pattern[PATH_MAX_LEN + 4];
    if (!resolve(s, (arg[0] && arg[0] != '-') ? arg : ".", ftp, dos)) {
        reply(s, "550 Bad path");
        return;
    }
    snprintf(pattern, sizeof(pattern), "%s%s*", dos, dos[strlen(dos) - 1] == '\\' ? "" : "\\");
    int fd = accept_data(s);
    if (fd < 0) {
        return;
    }
    reply(s, "150 Listing");
    WIN32_FIND_DATAA fd_data;
    HANDLE h = FindFirstFileA(pattern, &fd_data);
    if (h != INVALID_HANDLE_VALUE) {
        do {
            char line[400];
            int dir = (fd_data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0;
            int n = names_only ? snprintf(line, sizeof(line), "%s\r\n", fd_data.cFileName)
                               : snprintf(line, sizeof(line), "%crw-r--r-- 1 xbox xbox %10lu Jan  1  2000 %s\r\n",
                                          dir ? 'd' : '-', (unsigned long)fd_data.nFileSizeLow, fd_data.cFileName);
            if (n > 0 && !send_all(fd, line, n < (int)sizeof(line) ? n : (int)sizeof(line) - 1)) {
                break;
            }
        } while (FindNextFileA(h, &fd_data));
        FindClose(h);
    }
    lwip_close(fd);
    reply(s, "226 Done");
}

static void cmd_retr(session_t *s, const char *arg)
{
    char ftp[PATH_MAX_LEN], dos[PATH_MAX_LEN];
    HANDLE f = resolve(s, arg, ftp, dos)
                   ? CreateFileA(dos, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, NULL)
                   : INVALID_HANDLE_VALUE;
    if (f == INVALID_HANDLE_VALUE) {
        reply(s, "550 Can't open %s", arg);
        return;
    }
    int fd = accept_data(s);
    if (fd < 0) {
        CloseHandle(f);
        return;
    }
    reply(s, "150 Sending %s", ftp);
    DWORD n;
    int ok = 1;
    while (ok && ReadFile(f, io, sizeof(io), &n, NULL) && n > 0) {
        ok = send_all(fd, io, n);
    }
    CloseHandle(f);
    lwip_close(fd);
    reply(s, ok ? "226 Done" : "426 Transfer aborted");
}

// Receive into "<file>.part", then move it into place, so a cut-off upload never replaces a
// working file. A new copy of the running XBE is checked, installed and started.
static void cmd_stor(session_t *s, const char *arg)
{
    char ftp[PATH_MAX_LEN], dos[PATH_MAX_LEN], part[PATH_MAX_LEN + 8];
    if (!resolve(s, arg, ftp, dos) || is_dir(dos)) {
        reply(s, "553 Bad file name");
        return;
    }
    snprintf(part, sizeof(part), "%s.part", dos);
    HANDLE f = CreateFileA(part, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (f == INVALID_HANDLE_VALUE) {
        reply(s, "550 Can't create %s", ftp);
        return;
    }
    int fd = accept_data(s);
    if (fd < 0) {
        CloseHandle(f);
        DeleteFileA(part);
        return;
    }
    reply(s, "150 Receiving %s", ftp);
    int ok = 1, n = -1;
    uint8_t head[16];
    uint32_t total = 0;
    while (ok && readable(fd, DATA_TIMEOUT_S) && (n = lwip_recv(fd, io, sizeof(io), 0)) > 0) {
        DWORD w = 0;
        if (total < sizeof(head)) {
            uint32_t k = (uint32_t)n < sizeof(head) - total ? (uint32_t)n : sizeof(head) - total;
            memcpy(head + total, io, k);
        }
        ok = WriteFile(f, io, n, &w, NULL) && w == (DWORD)n;
        total += n;
    }
    int clean_end = ok && n == 0; // the client closed the data connection: upload complete
    lwip_close(fd);
    CloseHandle(f);
    if (!clean_end) {
        DeleteFileA(part);
        reply(s, "426 Upload incomplete; nothing was changed");
        return;
    }

    if (is_self(dos)) {
        if (!looks_like_xbe(head, total) || !install_xbe(part)) {
            DeleteFileA(part);
            reply(s, "550 Not a valid XBE, or it couldn't be installed; the old one is still in place");
            return;
        }
        reply(s, "226 Installed; xidpipe is restarting");
        say("New xidpipe uploaded over FTP. Restarting...");
        lwip_close(s->ctrl);
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
    char ftp[PATH_MAX_LEN], dos[PATH_MAX_LEN];

    if (!_stricmp(cmd, "USER")) {
        reply(s, "331 Any password works");
    } else if (!_stricmp(cmd, "PASS")) {
        reply(s, "230 Logged in");
    } else if (!_stricmp(cmd, "SYST")) {
        reply(s, "215 UNIX Type: L8");
    } else if (!_stricmp(cmd, "FEAT")) {
        reply(s, "211-Features:\r\n SIZE\r\n EPSV\r\n PASV\r\n UTF8\r\n211 End");
    } else if (!_stricmp(cmd, "OPTS") || !_stricmp(cmd, "TYPE") || !_stricmp(cmd, "MODE") ||
               !_stricmp(cmd, "STRU") || !_stricmp(cmd, "NOOP")) {
        reply(s, "200 OK"); // transfers are always binary
    } else if (!_stricmp(cmd, "PWD") || !_stricmp(cmd, "XPWD")) {
        reply(s, "257 \"%s\" is the current folder", s->cwd);
    } else if (!_stricmp(cmd, "CWD") || !_stricmp(cmd, "XCWD") || !_stricmp(cmd, "CDUP")) {
        if (resolve(s, !_stricmp(cmd, "CDUP") ? ".." : arg, ftp, dos) && (ftp[1] == 0 || is_dir(dos))) {
            snprintf(s->cwd, sizeof(s->cwd), "%s", ftp);
            reply(s, "250 Now in %s", s->cwd);
        } else {
            reply(s, "550 No such folder");
        }
    } else if (!_stricmp(cmd, "PASV") || !_stricmp(cmd, "EPSV")) {
        int port;
        if (!open_pasv(s, &port)) {
            reply(s, "425 Can't open a data port");
        } else if (!_stricmp(cmd, "EPSV")) {
            reply(s, "229 Entering Extended Passive Mode (|||%d|)", port);
        } else {
            uint32_t ip = ip4_addr_get_u32(netif_ip4_addr(g_pnetif));
            reply(s, "227 Entering Passive Mode (%u,%u,%u,%u,%d,%d)", ip & 0xff, (ip >> 8) & 0xff, (ip >> 16) & 0xff,
                  ip >> 24, port >> 8, port & 0xff);
        }
    } else if (!_stricmp(cmd, "LIST") || !_stricmp(cmd, "NLST")) {
        cmd_list(s, arg, !_stricmp(cmd, "NLST"));
    } else if (!_stricmp(cmd, "RETR")) {
        cmd_retr(s, arg);
    } else if (!_stricmp(cmd, "STOR")) {
        cmd_stor(s, arg);
    } else if (!_stricmp(cmd, "SIZE")) {
        HANDLE f = resolve(s, arg, ftp, dos) ? CreateFileA(dos, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING,
                                                           FILE_ATTRIBUTE_NORMAL, NULL)
                                             : INVALID_HANDLE_VALUE;
        if (f == INVALID_HANDLE_VALUE) {
            reply(s, "550 No such file");
        } else {
            reply(s, "213 %lu", (unsigned long)GetFileSize(f, NULL));
            CloseHandle(f);
        }
    } else if (!_stricmp(cmd, "DELE")) {
        int ok = resolve(s, arg, ftp, dos) && !is_self(dos) && DeleteFileA(dos);
        reply(s, ok ? "250 Deleted" : "550 Can't delete that (the running xidpipe can only be replaced)");
    } else if (!_stricmp(cmd, "MKD") || !_stricmp(cmd, "XMKD")) {
        int ok = resolve(s, arg, ftp, dos) && CreateDirectoryA(dos, NULL);
        if (ok) {
            reply(s, "257 \"%s\" created", ftp);
        } else {
            reply(s, "550 Can't create that folder");
        }
    } else if (!_stricmp(cmd, "RMD") || !_stricmp(cmd, "XRMD")) {
        int ok = resolve(s, arg, ftp, dos) && ftp[1] != 0 && RemoveDirectoryA(dos);
        reply(s, ok ? "250 Removed" : "550 Can't remove that folder");
    } else if (!_stricmp(cmd, "RNFR")) {
        if (resolve(s, arg, ftp, dos) && !is_self(dos) && GetFileAttributesA(dos) != INVALID_FILE_ATTRIBUTES) {
            snprintf(s->rnfr, sizeof(s->rnfr), "%s", dos);
            reply(s, "350 Ready for RNTO");
        } else {
            reply(s, "550 Can't rename that");
        }
    } else if (!_stricmp(cmd, "RNTO")) {
        int ok = s->rnfr[0] && resolve(s, arg, ftp, dos) && !is_self(dos) && MoveFileA(s->rnfr, dos);
        s->rnfr[0] = 0;
        reply(s, ok ? "250 Renamed" : "550 Can't rename that");
    } else if (!_stricmp(cmd, "PORT") || !_stricmp(cmd, "EPRT")) {
        reply(s, "502 Use passive mode");
    } else {
        reply(s, "502 Not supported");
    }
}

static void session(int ctrl)
{
    static session_t s;
    memset(&s, 0, sizeof(s));
    s.ctrl = ctrl;
    s.pasv = -1;
    strcpy(s.cwd, "/");
    reply(&s, "220 xidpipe " XIDPIPE_VERSION " FTP (this xidpipe folder only)");

    for (;;) {
        if (!readable(ctrl, IDLE_TIMEOUT_S)) {
            break;
        }
        int n = lwip_recv(ctrl, s.line + s.line_len, sizeof(s.line) - 1 - s.line_len, 0);
        if (n <= 0) {
            break;
        }
        s.line_len += n;
        s.line[s.line_len] = 0;
        char *eol;
        while ((eol = strchr(s.line, '\n')) != NULL) {
            *eol = 0;
            if (eol > s.line && eol[-1] == '\r') {
                eol[-1] = 0;
            }
            char *cmd = s.line, *arg = strchr(s.line, ' ');
            if (arg != NULL) {
                *arg++ = 0;
            } else {
                arg = "";
            }
            if (!_stricmp(cmd, "QUIT")) {
                reply(&s, "221 Bye");
                close_pasv(&s);
                return;
            }
            handle(&s, cmd, arg);
            size_t used = (size_t)(eol + 1 - s.line);
            memmove(s.line, eol + 1, s.line_len - used + 1);
            s.line_len -= used;
        }
        if (s.line_len >= sizeof(s.line) - 1) {
            s.line_len = 0; // overlong line: drop it
        }
    }
    close_pasv(&s);
}

static DWORD WINAPI ftp_thread(LPVOID arg)
{
    (void)arg;
    int lfd = lwip_socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in a;
    memset(&a, 0, sizeof(a));
    a.sin_family = AF_INET;
    a.sin_port = lwip_htons(FTP_PORT);
    a.sin_addr.s_addr = INADDR_ANY;
    if (lfd < 0 || lwip_bind(lfd, (struct sockaddr *)&a, sizeof(a)) != 0 || lwip_listen(lfd, 1) != 0) {
        say("FTP server couldn't start on port %d.", FTP_PORT);
        return 0;
    }
    say("FTP server on port %d (this xidpipe folder only).", FTP_PORT);
    for (;;) {
        int c = lwip_accept(lfd, NULL, NULL);
        if (c < 0) {
            Sleep(100);
            continue;
        }
        session(c);
        lwip_close(c);
    }
}

void ftp_start(void)
{
    if (marker_exists("no-ftp")) {
        say("FTP server is off (no-ftp file).");
        return;
    }
    HANDLE t = CreateThread(NULL, 64 * 1024, ftp_thread, NULL, 0, NULL);
    if (t != NULL) {
        SetThreadPriority(t, THREAD_PRIORITY_BELOW_NORMAL);
        CloseHandle(t);
    }
}
