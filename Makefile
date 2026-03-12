# ── Build ──────────────────────────────────────────────────────────────

.PHONY: all build clean

all: install insmod

build: cli kmod

clean:
	cargo clean --manifest-path cli/Cargo.toml
	rm -rf kmod/build

# ── CLI ────────────────────────────────────────────────────────────────

.PHONY: cli install uninstall

cli:
	cargo build --release --manifest-path cli/Cargo.toml

install: cli
	sudo install -m 4755 -o root cli/target/release/agfs /usr/local/bin/agfs

uninstall:
	sudo rm -f /usr/local/bin/agfs

# ── Kernel module ─────────────────────────────────────────────────────

.PHONY: kmod insmod rmmod

KDIR ?= /lib/modules/$(shell uname -r)/build
KMOD_OUT := kmod/build/agfs.ko

kmod: $(KMOD_OUT)

$(KMOD_OUT): $(wildcard kmod/*.c kmod/*.h kmod/Kbuild)
	mkdir -p kmod/build
	cp kmod/Kbuild kmod/build/Kbuild
	$(MAKE) -C $(KDIR) M=$(CURDIR)/kmod/build modules

insmod: $(KMOD_OUT) rmmod
	sudo insmod $(KMOD_OUT)

rmmod:
	mount | awk '$$3 ~ /\.agfs\/mnt/ {print $$3}' | sort -r | while read mnt; do sudo umount "$$mnt" || true; done
	sudo rmmod agfs || true

# ── Test ───────────────────────────────────────────────────────────────

.PHONY: test test-unit test-integration

test: test-unit test-integration

test-unit:
	cargo test --lib --manifest-path cli/Cargo.toml

test-integration: install insmod
	cargo test --manifest-path cli/Cargo.toml --test integration -- --test-threads=1
