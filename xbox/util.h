// SPDX-License-Identifier: GPL-3.0-or-later
// Platform-free helpers for the FTP server and the update check, so they can be unit-tested on
// a PC (tests/util_test.c).
#ifndef XIDPIPE_UTIL_H
#define XIDPIPE_UTIL_H

#include <stddef.h>
#include <stdint.h>

#define XP_PATH_MAX 260

// Copy the value of HTTP header `name` (case-insensitive) into `out`. `hdrs` is the status line
// and headers, each ending in CRLF. Returns 1 if found and it fits.
int http_header(const char *hdrs, const char *name, char *out, size_t outlen);

// Undo HTTP chunked transfer encoding in place. Fails unless the final 0-size chunk is present.
int http_dechunk(char *b, size_t len, size_t *out_len);

// Value of the first "key": "string" pair in a JSON document.
int json_string(const char *json, const char *key, char *out, size_t outlen);

// Status code from an HTTP status line ("HTTP/1.1 200 OK" -> 200), or 0 if it isn't one.
int http_status(const char *response);

// 1 if version `tag` ("v1.2.3" or "1.2.3") is newer than `mine`.
int version_newer(const char *tag, const char *mine);

// Unix time of a date in __DATE__ format ("Nov 18 2026"), at midnight UTC.
int64_t date_to_unix(const char *date);

// Map FTP path `arg` (absolute, or relative to folder `cwd`) onto the Xbox's drives the way Xbox
// dashboards do: "/" is the list of drives, "/E/Apps" is "E:\Apps". "." and ".." are resolved
// and stop at "/"; the first part must be a single drive letter; ':' is rejected (no "C:" tricks
// or alternate streams). Both outputs are XP_PATH_MAX bytes.
enum { FTP_BAD = 0, FTP_PATH = 1, FTP_ROOT = 2 };
int ftp_resolve(const char *cwd, const char *arg, char *ftp_out, char *dos_out);

#endif
