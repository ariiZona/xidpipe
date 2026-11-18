# Build: from an nxdk shell (nxdk/bin/activate), run `make` here.
# Output: bin/default.xbe and xidpipe.iso
XBE_TITLE = xidpipe
GEN_XISO = $(XBE_TITLE).iso
SRCS = $(CURDIR)/main.c $(CURDIR)/ftp.c $(CURDIR)/update.c $(CURDIR)/util.c $(CURDIR)/menu.c
NXDK_NET = y
NXDK_DIR ?= $(CURDIR)/nxdk

# One version for both apps: taken from pc/Cargo.toml.
XIDPIPE_VERSION := $(shell sed -n 's/^version *= *"\([^"]*\)".*/\1/p' $(CURDIR)/../pc/Cargo.toml | head -n 1)
# GitHub "owner/name" the update check looks at; forks can override: make XIDPIPE_REPO=me/xidpipe
XIDPIPE_REPO ?= ariiZona/xidpipe

# BearSSL (TLS for the update check), built as a library so only what's used gets linked.
# Its OS and CPU feature detection is switched off: the Xbox has no OS RNG or clock API and a
# Pentium III has no AES-NI, SSE2 or RDRAND. update.c supplies entropy and the time itself.
BEARSSL_DIR = $(CURDIR)/bearssl
# The x86 intrinsics variants (AES-NI, PCLMUL, SSE2) are left out: a Pentium III can't run them,
# and their headers clash with nxdk. sysrng.c is replaced by update.c's br_prng_seeder_system().
BEARSSL_SKIP = hash/ghash_pclmul.c symcipher/chacha20_sse2.c rand/sysrng.c \
	$(notdir $(wildcard $(BEARSSL_DIR)/src/symcipher/aes_x86ni*.c))
BEARSSL_SRCS = $(filter-out $(addprefix %/,$(BEARSSL_SKIP)), \
	$(wildcard $(BEARSSL_DIR)/src/*.c $(BEARSSL_DIR)/src/*/*.c))
BEARSSL_OBJS = $(addsuffix .obj, $(basename $(BEARSSL_SRCS)))
CFLAGS += -I$(BEARSSL_DIR)/inc \
	-DBR_USE_URANDOM=0 -DBR_USE_GETENTROPY=0 -DBR_USE_WIN32_RAND=0 \
	-DBR_USE_UNIX_TIME=0 -DBR_USE_WIN32_TIME=0 \
	-DBR_AES_X86NI=0 -DBR_SSE2=0 -DBR_RDRAND=0 -DBR_POWER8=0 \
	-DXIDPIPE_VERSION=\"$(XIDPIPE_VERSION)\" -DXIDPIPE_REPO=\"$(XIDPIPE_REPO)\"
$(BEARSSL_OBJS): CFLAGS += -I$(BEARSSL_DIR)/src

include $(NXDK_DIR)/Makefile

$(CURDIR)/bearssl.lib: $(BEARSSL_OBJS)
main.exe: $(CURDIR)/bearssl.lib

# The version reaches the code through CFLAGS, which make doesn't track: rebuild on a version bump.
$(CURDIR)/main.obj $(CURDIR)/ftp.obj $(CURDIR)/update.obj $(CURDIR)/menu.obj: $(CURDIR)/../pc/Cargo.toml

CLEANRULES += clean-bearssl
.PHONY: clean-bearssl
clean-bearssl:
	$(VE)rm -f $(BEARSSL_OBJS) $(BEARSSL_OBJS:.obj=.c.d) $(CURDIR)/bearssl.lib
