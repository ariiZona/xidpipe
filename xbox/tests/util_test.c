// SPDX-License-Identifier: GPL-3.0-or-later
// Unit tests for util.c, run on a PC: cc -Wall -Wextra -o util_test xbox/tests/util_test.c xbox/util.c
#include "../util.h"
#include <stdio.h>
#include <string.h>

static int failures;
#define CHECK(cond)                                                                                                    \
    do {                                                                                                               \
        if (!(cond)) {                                                                                                 \
            printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond);                                                     \
            failures++;                                                                                                \
        }                                                                                                              \
    } while (0)

static void test_headers(void)
{
    const char *h = "HTTP/1.1 302 Found\r\nContent-Length: 0\r\nLOCATION:  https://x.example/a?b=c\r\n";
    char v[64];
    CHECK(http_header(h, "location", v, sizeof(v)) && !strcmp(v, "https://x.example/a?b=c"));
    CHECK(http_header(h, "content-length", v, sizeof(v)) && !strcmp(v, "0"));
    CHECK(!http_header(h, "content", v, sizeof(v))); // must match the whole name
    CHECK(!http_header(h, "etag", v, sizeof(v)));
    CHECK(!http_header(h, "location", v, 8)); // doesn't fit
}

static void test_dechunk(void)
{
    char b[] = "4\r\nWiki\r\n5;ext=1\r\npedia\r\n0\r\n\r\n";
    size_t n = 0;
    CHECK(http_dechunk(b, strlen(b), &n) && n == 9 && !memcmp(b, "Wikipedia", 9));
    char cut[] = "4\r\nWiki\r\n5\r\nped"; // connection dropped mid-chunk
    CHECK(!http_dechunk(cut, strlen(cut), &n));
    char nofinal[] = "4\r\nWiki\r\n"; // no terminating 0 chunk
    CHECK(!http_dechunk(nofinal, strlen(nofinal), &n));
    char huge[] = "ffffffff\r\nab\r\n0\r\n\r\n";
    CHECK(!http_dechunk(huge, strlen(huge), &n));
}

static void test_json(void)
{
    char v[32];
    CHECK(json_string("{\"url\":\"x\",\"tag_name\" : \"v0.2.0\",\"name\":\"y\"}", "tag_name", v, sizeof(v)) &&
          !strcmp(v, "v0.2.0"));
    CHECK(!json_string("{\"name\":\"y\"}", "tag_name", v, sizeof(v)));
    CHECK(!json_string("{\"tag_name\":\"v0.2.0-way-too-long-for-the-buffer\"}", "tag_name", v, 8));
}

static void test_versions(void)
{
    CHECK(version_newer("v0.2.0", "0.1.1"));
    CHECK(version_newer("v0.10.0", "0.9.9")); // numeric, not text, comparison
    CHECK(version_newer("1.0.0", "0.99.99"));
    CHECK(!version_newer("v0.1.1", "0.1.1"));
    CHECK(!version_newer("v0.1.0", "0.1.1"));
    CHECK(!version_newer("nightly", "0.1.1"));
}

static void test_dates(void)
{
    CHECK(date_to_unix("Jan  1 1970") == 0);
    CHECK(date_to_unix("Mar  1 2000") == 951868800);
    CHECK(date_to_unix("Nov 18 2026") == 1794960000);
}

static void test_resolve(void)
{
    char ftp[XP_PATH_MAX], dos[XP_PATH_MAX];
    // "/" is the list of drives; "/E/..." is a path on E:
    CHECK(ftp_resolve("/", ".", ftp, dos) == FTP_ROOT && !strcmp(ftp, "/"));
    CHECK(ftp_resolve("/", "E/Apps", ftp, dos) == FTP_PATH && !strcmp(ftp, "/E/Apps") && !strcmp(dos, "E:\\Apps"));
    CHECK(ftp_resolve("/", "e", ftp, dos) == FTP_PATH && !strcmp(ftp, "/E") && !strcmp(dos, "E:\\"));
    CHECK(ftp_resolve("/E/a/b", "../c", ftp, dos) == FTP_PATH && !strcmp(ftp, "/E/a/c") && !strcmp(dos, "E:\\a\\c"));
    CHECK(ftp_resolve("/E/x", "/C/xboxdash.xbe", ftp, dos) == FTP_PATH && !strcmp(dos, "C:\\xboxdash.xbe"));
    CHECK(ftp_resolve("/E", "/F/./y//z", ftp, dos) == FTP_PATH && !strcmp(ftp, "/F/y/z"));
    // ".." stops at the drive list, however it's written
    CHECK(ftp_resolve("/E", "..", ftp, dos) == FTP_ROOT);
    CHECK(ftp_resolve("/", "../../..", ftp, dos) == FTP_ROOT);
    CHECK(ftp_resolve("/E/a", "..\\..\\..\\C\\x", ftp, dos) == FTP_PATH && !strcmp(dos, "C:\\x"));
    // The first part must be one drive letter
    CHECK(ftp_resolve("/", "Apps", ftp, dos) == FTP_BAD);
    CHECK(ftp_resolve("/", "/EE/x", ftp, dos) == FTP_BAD);
    CHECK(ftp_resolve("/", "/1/x", ftp, dos) == FTP_BAD);
    // No "C:" forms or alternate streams
    CHECK(ftp_resolve("/", "C:\\xboxdash.xbe", ftp, dos) == FTP_BAD);
    CHECK(ftp_resolve("/E", "file.txt:stream", ftp, dos) == FTP_BAD);
    char longname[400];
    memset(longname, 'a', sizeof(longname) - 1);
    longname[0] = 'E';
    longname[1] = '/';
    longname[sizeof(longname) - 1] = 0;
    CHECK(ftp_resolve("/", longname, ftp, dos) == FTP_BAD);
}

int main(void)
{
    test_headers();
    test_dechunk();
    test_json();
    test_versions();
    test_dates();
    test_resolve();
    if (failures) {
        printf("%d check(s) failed\n", failures);
        return 1;
    }
    printf("all util tests passed\n");
    return 0;
}
