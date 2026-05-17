SHELL := /bin/bash
ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/..)
OUT_DIR := $(ROOT)/lib
DYLIB := $(OUT_DIR)/rs-facetime-bridge-helper.dylib
SRC := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))/RsFacetimeInjected.m
ARCH ?= arm64e
MIN_MACOS ?= 14.0

.PHONY: all clean check

all: $(DYLIB)

$(DYLIB): $(SRC)
	@mkdir -p "$(OUT_DIR)"
	clang -dynamiclib -arch $(ARCH) \
		-mmacosx-version-min=$(MIN_MACOS) \
		-fobjc-arc -Wno-arc-performSelector-leaks \
		-framework Foundation -framework AppKit \
		-o "$(DYLIB)" "$(SRC)"
	codesign -fs - "$(DYLIB)" 2>/dev/null || true
	@echo "Built $(DYLIB)"

check: $(DYLIB)
	@file "$(DYLIB)"
	@otool -L "$(DYLIB)" | head -8

clean:
	rm -f "$(DYLIB)"
