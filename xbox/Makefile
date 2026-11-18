# Build: from an nxdk shell (nxdk/bin/activate), run `make` here.
# Output: bin/default.xbe and xidpipe.iso
XBE_TITLE = xidpipe
GEN_XISO = $(XBE_TITLE).iso
SRCS = $(CURDIR)/main.c
NXDK_NET = y
NXDK_DIR ?= $(CURDIR)/nxdk

include $(NXDK_DIR)/Makefile
