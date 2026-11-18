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

// 1 if version `tag` ("v1.2.3" or "1.2.3") is newer than `mine`.
int version_newer(const char *tag, const char *mine);

// Unix time of a date in __DATE__ format ("Nov 18 2026"), at midnight UTC.
int64_t date_to_unix(const char *date);

// Resolve FTP path `arg` (absolute, or relative to folder `cwd`) to a normalised FTP path and a
// DOS path on D:. Handles "." and ".."; never climbs above the root; rejects ':' (drives,
// streams). Both outputs are XP_PATH_MAX bytes. Returns 1 on success.
int ftp_resolve(const char *cwd, const char *arg, char *ftp_out, char *dos_out);

#endif
