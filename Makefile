# ── Build ──────────────────────────────────────────────────────────────

.PHONY: all build clean

all: install

build: cli kmod

clean:
	cargo clean
	rm -rf kmod/build

# ── CLI ────────────────────────────────────────────────────────────────

.PHONY: cli install uninstall

cli:
	cargo build --release

install: cli kmod
	sudo install -m 4755 -o root target/release/agfs /usr/local/bin/agfs

uninstall:
	sudo rm -f /usr/local/bin/agfs

# ── Kernel module ─────────────────────────────────────────────────────

.PHONY: kmod

KDIR ?= /lib/modules/$(shell uname -r)/build
KMOD_OUT := kmod/build/agfs.ko

kmod: $(KMOD_OUT)

$(KMOD_OUT): $(wildcard kmod/*.c kmod/*.h kmod/Kbuild)
	mkdir -p kmod/build
	cp kmod/Kbuild kmod/build/Kbuild
	$(MAKE) -j$(nproc) -C $(KDIR) M=$(CURDIR)/kmod/build modules

# ── Test ───────────────────────────────────────────────────────────────

.PHONY: test test-unit test-integration

test: test-unit test-integration

test-unit:
	cargo test --lib

test-integration: install
	agfs init
	cargo test --test integration -- --test-threads=1
