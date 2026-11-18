// SPDX-License-Identifier: GPL-3.0-or-later
#include "util.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int ascii_lower(int c)
{
    return c >= 'A' && c <= 'Z' ? c + 32 : c;
}

static int prefix_nocase(const char *s, const char *prefix, size_t n)
{
    for (size_t i = 0; i < n; i++) {
        if (s[i] == 0 || ascii_lower((unsigned char)s[i]) != ascii_lower((unsigned char)prefix[i])) {
            return 0;
        }
    }
    return 1;
}

int http_header(const char *hdrs, const char *name, char *out, size_t outlen)
{
    size_t nl = strlen(name);
    for (const char *line = strstr(hdrs, "\r\n"); line != NULL; line = strstr(line + 2, "\r\n")) {
        const char *l = line + 2;
        if (prefix_nocase(l, name, nl) && l[nl] == ':') {
            const char *v = l + nl + 1;
            while (*v == ' ' || *v == '\t') {
                v++;
            }
            const char *e = strstr(v, "\r\n");
            size_t vl = e ? (size_t)(e - v) : strlen(v);
            if (vl >= outlen) {
                return 0;
            }
            memcpy(out, v, vl);
            out[vl] = 0;
            return 1;
        }
    }
    return 0;
}

int http_dechunk(char *b, size_t len, size_t *out_len)
{
    size_t r = 0, w = 0;
    while (r < len) {
        char *end;
        unsigned long n = strtoul(b + r, &end, 16);
        char *crlf = strstr(end, "\r\n");
        if (end == b + r || crlf == NULL) {
            return 0;
        }
        r = (size_t)(crlf + 2 - b);
        if (n == 0) {
            *out_len = w;
            return 1;
        }
        if (n > len || r + n + 2 > len) {
            return 0;
        }
        memmove(b + w, b + r, n);
        w += n;
        r += n + 2;
    }
    return 0;
}

int json_string(const char *json, const char *key, char *out, size_t outlen)
{
    char pat[64];
    snprintf(pat, sizeof(pat), "\"%s\"", key);
    const char *p = strstr(json, pat);
    if (p == NULL || (p = strchr(p + strlen(pat), ':')) == NULL || (p = strchr(p, '"')) == NULL) {
        return 0;
    }
    p++;
    const char *e = strchr(p, '"');
    if (e == NULL || (size_t)(e - p) >= outlen) {
        return 0;
    }
    memcpy(out, p, (size_t)(e - p));
    out[e - p] = 0;
    return 1;
}

// No sscanf here or below: the Xbox's C library (pdclib) doesn't handle all of its formats.
static int parse_version(const char *s, int v[3])
{
    if (*s == 'v') {
        s++;
    }
    for (int i = 0; i < 3; i++) {
        char *end;
        long n = strtol(s, &end, 10);
        if (end == s || n < 0 || (i < 2 && *end != '.')) {
            return 0;
        }
        v[i] = (int)n;
        s = end + 1;
    }
    return 1;
}

int http_status(const char *response)
{
    if (strncmp(response, "HTTP/", 5) != 0) {
        return 0;
    }
    const char *sp = strchr(response, ' ');
    if (sp == NULL) {
        return 0;
    }
    char *end;
    long code = strtol(sp + 1, &end, 10);
    return (end != sp + 1 && code >= 100 && code <= 999) ? (int)code : 0;
}

int version_newer(const char *tag, const char *mine)
{
    int a[3], b[3];
    if (!parse_version(tag, a) || !parse_version(mine, b)) {
        return 0;
    }
    for (int i = 0; i < 3; i++) {
        if (a[i] != b[i]) {
            return a[i] > b[i];
        }
    }
    return 0;
}

int64_t date_to_unix(const char *date)
{
    static const char months[] = "JanFebMarAprMayJunJulAugSepOctNovDec";
    char mon[4] = {date[0], date[1], date[2], 0};
    const char *m = strstr(months, mon);
    int month = m ? (int)(m - months) / 3 + 1 : 1;
    int day = atoi(date + 4), year = atoi(date + 7);
    // Days from 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    int y = year - (month <= 2);
    int era = y / 400;
    int yoe = y - era * 400;
    int doy = (153 * (month + (month > 2 ? -3 : 9)) + 2) / 5 + day - 1;
    int doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    return ((int64_t)era * 146097 + doe - 719468) * 86400;
}

int ftp_resolve(const char *cwd, const char *arg, char *ftp_out, char *dos_out)
{
    char in[2 * XP_PATH_MAX];
    snprintf(in, sizeof(in), "%s/%s", arg[0] == '/' ? "" : cwd, arg);
    char norm[XP_PATH_MAX - 8]; // room left for a ".part" suffix
    size_t nl = 0;
    const char *p = in;
    while (*p) {
        while (*p == '/' || *p == '\\') {
            p++;
        }
        const char *start = p;
        while (*p && *p != '/' && *p != '\\') {
            p++;
        }
        size_t len = (size_t)(p - start);
        if (len == 0 || (len == 1 && start[0] == '.')) {
            continue;
        }
        if (len == 2 && start[0] == '.' && start[1] == '.') {
            while (nl > 0 && norm[nl - 1] != '/') {
                nl--;
            }
            if (nl > 0) {
                nl--; // drop the '/' too
            }
            continue;
        }
        if (memchr(start, ':', len) != NULL || nl + 1 + len >= sizeof(norm)) {
            return FTP_BAD;
        }
        norm[nl++] = '/';
        memcpy(norm + nl, start, len);
        nl += len;
    }
    norm[nl] = 0;
    if (nl == 0) {
        snprintf(ftp_out, XP_PATH_MAX, "/");
        dos_out[0] = 0;
        return FTP_ROOT;
    }
    // "/E" or "/E/...": the first part is the drive letter.
    char drive = norm[1];
    if (!((drive >= 'A' && drive <= 'Z') || (drive >= 'a' && drive <= 'z')) || (norm[2] != 0 && norm[2] != '/')) {
        return FTP_BAD;
    }
    if (drive >= 'a') {
        drive = (char)(drive - 32);
    }
    norm[1] = drive;
    snprintf(ftp_out, XP_PATH_MAX, "%s", norm);
    snprintf(dos_out, XP_PATH_MAX, "%c:%s", drive, norm[2] ? norm + 2 : "\\");
    for (char *c = dos_out; *c; c++) {
        if (*c == '/') {
            *c = '\\';
        }
    }
    return FTP_PATH;
}
