// SPDX-License-Identifier: GPL-3.0-or-later
// Shared between the Xbox app's parts: main.c (controllers, PC link), ftp.c, update.c.
#ifndef XIDPIPE_H
#define XIDPIPE_H

#include <stdint.h>

#ifndef XIDPIPE_VERSION
#define XIDPIPE_VERSION "0.0.0" // the Makefile passes the version from pc/Cargo.toml
#endif
#ifndef XIDPIPE_REPO
#define XIDPIPE_REPO "ariiZona/xidpipe" // GitHub "owner/name" the update check looks at
#endif

// Thread-safe screen output (debugPrint isn't safe to call from several threads at once).
void say(const char *fmt, ...);

// The running XBE: its DOS path on the D: drive (D: is the folder it was started from).
const char *self_xbe_path(void);
const char *self_xbe_name(void);

// True if an empty marker file with this name exists next to the XBE (used for opt-outs).
int marker_exists(const char *name);

// Replace the running XBE with `new_path` (a complete, already verified file on D:).
// The old file is kept until the new one is in place and put back if anything fails.
// Returns 1 on success.
int install_xbe(const char *new_path);

// Restart into the (possibly just replaced) XBE. Does not return.
void relaunch_self(void);

// Basic sanity check for an XBE image: magic and a plausible size.
int looks_like_xbe(const uint8_t *data, uint32_t len);

void ftp_start(void);
void update_start(void);

// Update state, shared by update.c (sets it) and main.c (asks the user, then installs).
enum { UPDATE_NONE, UPDATE_READY, UPDATE_INSTALLING };
extern volatile int update_state;
extern char update_version[32]; // version that's downloaded and ready
extern char update_file[260];   // where it was downloaded

#endif
